use std::ops::ControlFlow;
use std::time::Duration;
use std::time::Instant;

use tracing::debug;
use tracing::trace;

/// Result of advancing a candidate past deterministic decisions.
pub enum AdvanceResult<D> {
    /// Reached a decision point.
    Decision(D),
    /// All decisions resolved; candidate is complete.
    Complete,
}

/// Search space for BULB (Beam search Using Limited discrepancy
/// Backtracking; Furcy 2006, "Limited Discrepancy Beam Search").
///
/// Operates on a single mutable candidate via checkpoint/rollback instead
/// of cloning; `snapshot()` is called only to save the best complete candidate.
///
/// # Example
///
/// A two-decision space where the option that probes cheapest at the
/// first decision forces an expensive follow-up. The greedy pass (fuel 0)
/// falls into the trap; one discrepancy iteration revisits the first
/// decision and escapes it. This is the design doc's "Iteration 0 /
/// Iteration 1" walkthrough in miniature.
///
/// ```
/// use apollo_federation::query_plan::incremental_planner::bulb_search::*;
/// use std::cell::Cell;
///
/// struct Trap {
///     effort: Cell<u64>,
/// }
///
/// impl BulbSearchSpace for Trap {
///     type Candidate = Vec<u64>;
///     type Decision = Vec<u64>;
///     type Choice = u64;
///     type Checkpoint = usize;
///
///     fn advance(&self, picks: &mut Vec<u64>) -> AdvanceResult<Vec<u64>> {
///         if picks.len() < 2 {
///             AdvanceResult::Decision(picks.clone())
///         } else {
///             AdvanceResult::Complete
///         }
///     }
///     fn options(&self, prefix: &Vec<u64>) -> Vec<u64> {
///         match prefix.as_slice() {
///             [] => vec![0, 1],  // 0 probes cheaper at this decision...
///             [0] => vec![10],   // ...but forces an expensive follow-up
///             [1] => vec![0],    // while 1 unlocks a free one
///             _ => vec![],
///         }
///     }
///     fn apply(&self, picks: &mut Vec<u64>, _: &Vec<u64>, choice: &u64) {
///         self.effort.set(self.effort.get() + 1);
///         picks.push(*choice);
///     }
///     fn checkpoint(&self, picks: &Vec<u64>) -> usize {
///         picks.len()
///     }
///     fn rollback(&self, picks: &mut Vec<u64>, cp: usize) {
///         picks.truncate(cp);
///     }
///     fn snapshot(&self, picks: &Vec<u64>) -> Vec<u64> {
///         picks.clone()
///     }
///     fn cost(&self, picks: &Vec<u64>) -> f64 {
///         picks.iter().sum::<u64>() as f64
///     }
///     fn effort(&self, _: &Vec<u64>) -> u64 {
///         self.effort.get()
///     }
/// }
///
/// let config = |fuel| BulbConfig {
///     beam_width: 2,
///     fuel,
///     timeout: None,
/// };
///
/// // fuel=0: greedy only, falls into the trap.
/// let space = Trap { effort: Cell::new(0) };
/// let (greedy, _) = bulb_search(&space, vec![], config(0), None);
/// assert_eq!(greedy, vec![0, 10]);
///
/// // With fuel, a discrepancy iteration revisits the first decision
/// // and finds the cheaper plan.
/// let space = Trap { effort: Cell::new(0) };
/// let (best, _) = bulb_search(&space, vec![], config(100), None);
/// assert_eq!(best, vec![1, 0]);
/// ```
pub trait BulbSearchSpace {
    type Candidate;
    type Decision: Clone;
    type Choice: Clone;
    type Checkpoint: Clone;

    /// Advance past all deterministic (single-option) decisions in place,
    /// returning the next multi-option decision point or `Complete`.
    fn advance(&self, candidate: &mut Self::Candidate) -> AdvanceResult<Self::Decision>;

    /// Enumerate options for a decision, best first.
    fn options(&self, decision: &Self::Decision) -> Vec<Self::Choice>;

    /// Apply a choice in place at the decision point (`advance` already
    /// called): pops the decision and commits the choice.
    fn apply(
        &self,
        candidate: &mut Self::Candidate,
        decision: &Self::Decision,
        choice: &Self::Choice,
    );

    /// Save the candidate's current state for later rollback. O(1).
    fn checkpoint(&self, candidate: &Self::Candidate) -> Self::Checkpoint;

    /// Restore a previously saved checkpoint, undoing all mutations since.
    /// Checkpoints must be used in LIFO order.
    fn rollback(&self, candidate: &mut Self::Candidate, cp: Self::Checkpoint);

    /// Full deep clone; used only to save the best complete candidate.
    fn snapshot(&self, candidate: &Self::Candidate) -> Self::Candidate;

    /// Heuristic cost of a (possibly partial) candidate. Lower is better.
    fn cost(&self, candidate: &Self::Candidate) -> f64;

    /// Whether a completed candidate satisfies the full request. Only
    /// complete candidates are recorded as results; incomplete completions
    /// (dead ends that gave up on part of the request) still update the
    /// incumbent cost bound for pruning. Spaces must cost every incomplete
    /// candidate above every complete one, so an incomplete completion can
    /// never out-prune a reachable complete plan. The default (always true)
    /// suits spaces where every completion satisfies the request.
    fn is_complete(&self, candidate: &Self::Candidate) -> bool {
        let _ = candidate;
        true
    }

    /// Monotonic total work spent on this candidate across the whole
    /// search, including rolled-back work; the budget is the effort at the
    /// first complete candidate plus `fuel`. The default (always 0) disables
    /// effort budgeting — do NOT combine it with `timeout: None` unless the
    /// space is finite: there is deliberately no "no-improvement" stop (an
    /// iteration can end completion-free while deeper discrepancy levels
    /// still hold improvements), so only `!alternatives_existed` would end
    /// the loop.
    fn effort(&self, candidate: &Self::Candidate) -> u64 {
        let _ = candidate;
        0
    }
}

#[derive(Debug, Clone)]
pub struct BulbConfig {
    /// B: children explored per decision. B=1 is greedy. The greedy pass
    /// (discrepancy=0) always uses B=1; later iterations use this value.
    pub beam_width: usize,
    /// Cap on search effort beyond the first complete candidate, in effort
    /// units (see [`BulbSearchSpace::effort`]). The search runs unbudgeted
    /// until a candidate satisfying [`BulbSearchSpace::is_complete`] is
    /// recorded; `fuel: 0` stops at the first complete candidate. When
    /// exhausted, the search returns the best complete candidate found so
    /// far.
    pub fuel: u64,
    /// Optional wall-clock limit, after which the best solution so far is
    /// returned, making the result machine-load dependent. Leave `None`
    /// (the default) for deterministic, fuel-bounded search; set it to cap
    /// the search at a surrounding request's deadline.
    pub timeout: Option<Duration>,
}

impl Default for BulbConfig {
    fn default() -> Self {
        Self {
            beam_width: 16,
            fuel: 5_000,
            timeout: None,
        }
    }
}

/// Statistics from a BULB search run.
pub struct BulbStats {
    /// Complete candidates evaluated.
    pub evaluated_plans: usize,
    /// Decision points expanded (advanced to and scored).
    pub expansions: usize,
    /// Total effort spent (see [`BulbSearchSpace::effort`]).
    pub effort: u64,
    /// Effort already spent when the first complete candidate was recorded;
    /// fuel consumption is measured from this point. `None` when no
    /// complete candidate was ever found.
    pub first_complete_effort: Option<u64>,
    /// Terminated by the wall-clock timeout.
    pub timed_out: bool,
    /// Terminated by cooperative cancellation.
    pub cancelled: bool,
}

/// Run BULB search on the given search space.
///
/// A DFS variant of Furcy 2006 "Limited Discrepancy Beam Search" adapted
/// for undo-based operation: instead of a beam of B cloned states per
/// layer, a single mutable candidate with checkpoint/rollback. At each
/// decision point, all options are scored (apply -> cost -> rollback),
/// sorted, and the top `beam_width` explored via DFS. Scoring uses partial
/// cost (no advance past single-option decisions), keeping it O(B) per
/// decision point; completions are only found during exploration.
///
/// The outer loop increments the allowed discrepancies: iteration 0 is
/// greedy (B=1), each subsequent iteration permits one more deviation
/// (choosing a non-first slice).
pub fn bulb_search<S: BulbSearchSpace>(
    space: &S,
    mut initial: S::Candidate,
    config: BulbConfig,
    check_cancellation: Option<&dyn Fn() -> ControlFlow<()>>,
) -> (S::Candidate, BulbStats) {
    let b = config.beam_width.max(1);
    let fuel = config.fuel;
    let mut progress = BulbProgress {
        deadline: config.timeout.map(|t| Instant::now() + t),
        check_cancellation,
        was_cancelled: false,
        completions: 0,
        expansions: 0,
        fuel,
        effort_budget: None,
        first_complete_effort: None,
        deepest_stack: 0,
        best: None,
        best_cost: f64::MAX,
    };
    let mut timed_out = false;

    let initial_cp = space.checkpoint(&initial);

    for max_disc in 0.. {
        // Differs from the paper (B at every iteration): B=1 for the
        // greedy pass avoids wide beam allocations on deep queries.
        let effective_b = if max_disc == 0 { 1 } else { b };
        trace!(
            max_disc,
            effective_b, progress.completions, "starting BULB probe iteration",
        );

        let alternatives_existed =
            bulb_probe(space, &mut initial, max_disc, effective_b, &mut progress);

        // Restore to initial state for the next iteration. The effort
        // budget is armed by `record_completion` when the first complete
        // candidate lands; until then the search runs unbudgeted.
        space.rollback(&mut initial, initial_cp.clone());

        trace!(
            max_disc,
            total_completions = progress.completions,
            alternatives_existed,
            progress.best_cost,
            "BULB probe iteration done",
        );

        if progress.exhausted(space.effort(&initial)) {
            timed_out = progress.out_of_time();
            if progress.was_cancelled {
                debug!(max_disc, progress.completions, "BULB search cancelled");
            } else if timed_out {
                debug!(max_disc, progress.completions, "BULB search timed out");
            } else {
                debug!(max_disc, progress.completions, "BULB search exhausted fuel");
            }
            break;
        }
        if !alternatives_existed {
            break;
        }
        // A path with d decision points can absorb at most d discrepancies
        // (one alternative slice each), so once the budget covers the
        // deepest stack seen, every combination has been explored. Without
        // this, a space with alternatives but no complete candidate would
        // iterate forever: the effort budget stays unarmed until a complete
        // candidate lands.
        if max_disc >= progress.deepest_stack {
            break;
        }
    }

    let result = progress.best.unwrap_or_else(|| space.snapshot(&initial));
    (
        result,
        BulbStats {
            evaluated_plans: progress.completions,
            expansions: progress.expansions,
            effort: space.effort(&initial),
            first_complete_effort: progress.first_complete_effort,
            timed_out,
            cancelled: progress.was_cancelled,
        },
    )
}

/// Mutable state shared across the iterative DFS.
struct BulbProgress<'a, C> {
    deadline: Option<Instant>,
    check_cancellation: Option<&'a dyn Fn() -> ControlFlow<()>>,
    was_cancelled: bool,
    completions: usize,
    expansions: usize,
    fuel: u64,
    /// Cap on the candidate's monotonic effort counter (see
    /// [`BulbSearchSpace::effort`]): the effort at the first complete
    /// candidate plus `fuel`. `None` until the first complete candidate is
    /// recorded — fuel bounds optimization beyond a complete plan, never
    /// the search for one.
    effort_budget: Option<u64>,
    /// Effort at the moment the first complete candidate was recorded.
    first_complete_effort: Option<u64>,
    /// Deepest decision stack seen across all probe iterations. A path
    /// with d decision points can absorb at most d discrepancies, so once
    /// `max_disc` reaches this depth every discrepancy combination has been
    /// explored and further iterations are no-ops.
    deepest_stack: usize,
    /// Best complete candidate found so far — never an incomplete one.
    best: Option<C>,
    /// Incumbent prune bound: cheapest completion cost seen, complete or
    /// not. Incomplete completions cost above all complete ones (see
    /// [`BulbSearchSpace::is_complete`]), so this bound never prunes a path
    /// to a better complete plan.
    best_cost: f64,
}

impl<C> BulbProgress<'_, C> {
    fn out_of_time(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }

    fn cancelled(&mut self) -> bool {
        if self.was_cancelled {
            return true;
        }
        if self
            .check_cancellation
            .is_some_and(|check| check() == ControlFlow::Break(()))
        {
            self.was_cancelled = true;
            return true;
        }
        false
    }

    fn exhausted(&mut self, effort: u64) -> bool {
        self.effort_budget.is_some_and(|budget| effort >= budget)
            || self.out_of_time()
            || self.cancelled()
    }
}

/// One level of the BULB DFS, stored on an explicit stack instead of the
/// call stack so deeply nested queries don't overflow.
///
/// Each frame represents a decision point. The search descends by pushing
/// frames (one per decision encountered), and ascends by popping them when
/// all options at that level have been explored or pruned.
///
/// Options are pre-sorted by cost and arranged in exploration order at
/// construction time: alt slices (1..N) first, then the best slice (0).
/// This follows the paper's "backtrack alternatives before greedy" order.
struct BulbFrame<D, Ch, Cp> {
    decision: D,
    options: Vec<Ch>,
    /// (index into `options`, cost) in exploration order.
    /// `order[..alt_end]` are alt-slice options whose children get `disc - 1`;
    /// `order[alt_end..]` are best-slice options whose children get the full `disc`.
    /// Within each section, entries are sorted by ascending cost.
    order: Vec<(usize, f64)>,
    alt_end: usize,
    pos: usize,
    checkpoint: Cp,
    disc: usize,
    alternatives_existed: bool,
}

impl<D, Ch, Cp> BulbFrame<D, Ch, Cp> {
    fn new(
        decision: D,
        options: Vec<Ch>,
        scored: Vec<(usize, f64)>,
        checkpoint: Cp,
        disc: usize,
        bw: usize,
    ) -> Self {
        let alternatives_existed = scored.len() > bw;
        let best_end = bw.min(scored.len());

        // Alt slices first (indices bw..end of scored), then best slice
        // (indices 0..bw). When disc=0 or only one slice exists, skip
        // alts entirely.
        let (order, alt_end) = if disc > 0 && scored.len() > bw {
            let alt_count = scored.len() - best_end;
            let mut order = Vec::with_capacity(scored.len());
            order.extend_from_slice(&scored[best_end..]);
            order.extend_from_slice(&scored[..best_end]);
            (order, alt_count)
        } else {
            (scored[..best_end].to_vec(), 0)
        };

        trace!(
            beam_pool = ?order,
            alt_end,
            disc,
            bw,
            alternatives_existed,
            "beam candidate pool finalized for this decision",
        );

        Self {
            decision,
            options,
            order,
            alt_end,
            pos: 0,
            checkpoint,
            disc,
            alternatives_existed,
        }
    }

    /// Advance to the next option to explore at this decision, returning
    /// the option index and the discrepancy budget for child decisions.
    /// Alt-slice options whose cost exceeds the incumbent are pruned (they
    /// spend a discrepancy to enter, so skipping them saves fuel). The best
    /// slice is never pruned — it is the greedy path and may be the only
    /// route to a completion.
    fn next_option(&mut self, best_cost: f64) -> Option<(usize, usize)> {
        while self.pos < self.order.len() {
            let (opt_idx, cost) = self.order[self.pos];
            if self.pos < self.alt_end && cost >= best_cost {
                // Alt section is sorted; skip to the best section.
                self.pos = self.alt_end;
                continue;
            }
            let child_disc = if self.pos < self.alt_end {
                self.disc - 1
            } else {
                self.disc
            };
            self.pos += 1;
            return Some((opt_idx, child_disc));
        }
        None
    }
}

/// Bubble a frame's `alternatives_existed` flag up to its parent frame
/// (or to the probe-level result if no parent exists).
fn propagate_alternatives<D, Ch, Cp>(
    stack: &mut [BulbFrame<D, Ch, Cp>],
    result_alts: &mut bool,
    alts: bool,
) {
    if let Some(parent) = stack.last_mut() {
        parent.alternatives_existed |= alts;
    } else {
        *result_alts |= alts;
    }
}

/// Iterative DFS BULB probe. At each node:
///
/// 1. Advance past deterministic decisions to the next choice point.
/// 2. Score all options: apply -> cost -> rollback (no advance).
/// 3. Sort by cost, arrange into exploration order (alt slices first,
///    then best slice).
/// 4. Explore via DFS: disc=0 explores the best slice only; disc>0
///    explores alternative slices first (disc-1), then the best slice
///    (full disc).
///
/// Returns whether any decision point had more than one slice (a genuine
/// alternative to backtrack into).
fn bulb_probe<S: BulbSearchSpace>(
    space: &S,
    candidate: &mut S::Candidate,
    discrepancies: usize,
    beam_width: usize,
    progress: &mut BulbProgress<S::Candidate>,
) -> bool {
    let mut stack: Vec<BulbFrame<S::Decision, S::Choice, S::Checkpoint>> = Vec::new();
    let mut disc_budget = discrepancies;
    let mut result_alts = false;

    'search: loop {
        // Descend: advance to the next decision point, score options,
        // push a frame, apply the first option.
        let pushed = 'descend: {
            if progress.exhausted(space.effort(candidate)) {
                break 'descend false;
            }

            match space.advance(candidate) {
                AdvanceResult::Complete => {
                    if !progress.cancelled() {
                        record_completion(space, candidate, progress);
                    }
                    break 'descend false;
                }
                AdvanceResult::Decision(decision) => {
                    progress.expansions += 1;
                    let checkpoint = space.checkpoint(candidate);
                    let options = space.options(&decision);

                    let mut scored = score_options(
                        space,
                        candidate,
                        &decision,
                        &options,
                        &checkpoint,
                        progress.best.is_some(),
                        progress.best_cost,
                    );

                    if progress.exhausted(space.effort(candidate)) || scored.is_empty() {
                        break 'descend false;
                    }

                    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
                    let mut frame = BulbFrame::new(
                        decision,
                        options,
                        scored,
                        checkpoint,
                        disc_budget,
                        beam_width,
                    );

                    match frame.next_option(progress.best_cost) {
                        Some((opt_idx, child_disc)) => {
                            space.apply(candidate, &frame.decision, &frame.options[opt_idx]);
                            disc_budget = child_disc;
                            stack.push(frame);
                            progress.deepest_stack = progress.deepest_stack.max(stack.len());
                            true
                        }
                        None => {
                            propagate_alternatives(
                                &mut stack,
                                &mut result_alts,
                                frame.alternatives_existed,
                            );
                            false
                        }
                    }
                }
            }
        };

        if pushed {
            continue 'search;
        }

        // Ascend: rollback to the frame's checkpoint, try the next option.
        // If no options remain, pop the frame and try the parent.
        loop {
            let Some(frame) = stack.last_mut() else {
                return result_alts;
            };
            space.rollback(candidate, frame.checkpoint.clone());
            if !progress.exhausted(space.effort(candidate))
                && let Some((opt_idx, child_disc)) = frame.next_option(progress.best_cost)
            {
                disc_budget = child_disc;
                space.apply(candidate, &frame.decision, &frame.options[opt_idx]);
                continue 'search;
            }
            let frame = stack.pop().unwrap();
            propagate_alternatives(&mut stack, &mut result_alts, frame.alternatives_existed);
        }
    }
}

/// Score all options at a decision point: apply -> cost -> rollback per option.
/// Returns (option_index, cost) pairs for options that survive the incumbent prune.
fn score_options<S: BulbSearchSpace>(
    space: &S,
    candidate: &mut S::Candidate,
    decision: &S::Decision,
    options: &[S::Choice],
    checkpoint: &S::Checkpoint,
    has_incumbent: bool,
    best_cost: f64,
) -> Vec<(usize, f64)> {
    let mut scored: Vec<(usize, f64)> = Vec::with_capacity(options.len());

    for (i, choice) in options.iter().enumerate() {
        space.apply(candidate, decision, choice);
        let cost = space.cost(candidate);
        if !has_incumbent || cost < best_cost {
            scored.push((i, cost));
            trace!(
                option_index = i,
                cost,
                beam_pool = ?scored,
                "scored option, added to beam candidate pool",
            );
        } else {
            trace!(
                option_index = i,
                cost, best_cost, "scored option, pruned (>= incumbent best)",
            );
        }
        space.rollback(candidate, checkpoint.clone());
    }

    scored
}

/// Record a completed candidate. Any completion tightens the incumbent
/// prune bound, but only complete candidates (no abandoned work) are saved
/// as results, and the first one arms the fuel budget.
fn record_completion<S: BulbSearchSpace>(
    space: &S,
    candidate: &S::Candidate,
    progress: &mut BulbProgress<S::Candidate>,
) {
    let cost = space.cost(candidate);
    progress.completions += 1;
    let complete = space.is_complete(candidate);
    let improved = cost < progress.best_cost;
    debug!(
        completion = progress.completions,
        cost,
        prev_best = progress.best_cost,
        improved,
        complete,
        "candidate completed",
    );
    if improved {
        progress.best_cost = cost;
    }
    if complete {
        if progress.first_complete_effort.is_none() {
            let effort = space.effort(candidate);
            progress.first_complete_effort = Some(effort);
            progress.effort_budget = Some(effort.saturating_add(progress.fuel));
        }
        if improved {
            progress.best = Some(space.snapshot(candidate));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct LevelState {
        path: Vec<usize>,
        level: usize,
    }

    type CostFn = Box<dyn Fn(&[usize], usize) -> f64>;

    struct LevelSpace {
        options_per_level: Vec<Vec<usize>>,
        cost_fn: CostFn,
        effort: std::cell::Cell<u64>,
    }

    impl BulbSearchSpace for LevelSpace {
        type Candidate = LevelState;
        type Decision = usize;
        type Choice = usize;
        type Checkpoint = LevelState;

        fn advance(&self, candidate: &mut LevelState) -> AdvanceResult<usize> {
            if candidate.level < self.options_per_level.len() {
                AdvanceResult::Decision(candidate.level)
            } else {
                AdvanceResult::Complete
            }
        }

        fn options(&self, decision: &usize) -> Vec<usize> {
            let mut opts = self.options_per_level[*decision].clone();
            opts.sort();
            opts
        }

        fn apply(&self, candidate: &mut LevelState, _decision: &usize, choice: &usize) {
            self.effort.set(self.effort.get() + 1);
            candidate.path.push(*choice);
            candidate.level += 1;
        }

        fn checkpoint(&self, candidate: &LevelState) -> LevelState {
            candidate.clone()
        }

        fn rollback(&self, candidate: &mut LevelState, cp: LevelState) {
            *candidate = cp;
        }

        fn snapshot(&self, candidate: &LevelState) -> LevelState {
            candidate.clone()
        }

        fn effort(&self, _candidate: &LevelState) -> u64 {
            self.effort.get()
        }

        fn cost(&self, candidate: &LevelState) -> f64 {
            (self.cost_fn)(&candidate.path, candidate.level)
        }
    }

    fn initial() -> LevelState {
        LevelState {
            path: vec![],
            level: 0,
        }
    }

    fn default_timeout() -> Option<Duration> {
        Some(Duration::from_secs(30))
    }

    fn sum_space(options_per_level: Vec<Vec<usize>>) -> LevelSpace {
        LevelSpace {
            options_per_level,
            cost_fn: Box::new(|path, _level| path.iter().map(|&v| v as f64).sum()),
            effort: std::cell::Cell::new(0),
        }
    }

    #[test]
    fn greedy_finds_optimal_on_simple_space() {
        let space = sum_space(vec![vec![3, 1, 2], vec![5, 4]]);
        let config = BulbConfig {
            beam_width: 2,
            fuel: 100,
            timeout: default_timeout(),
        };
        let (result, stats) = bulb_search(&space, initial(), config, None);
        assert_eq!(result.path, vec![1, 4]);
        assert_eq!(space.cost(&result), 5.0);
        assert!(stats.evaluated_plans >= 1);
    }

    #[test]
    fn single_option_per_level_no_decision_points() {
        let space = sum_space(vec![vec![5], vec![3], vec![7]]);
        let config = BulbConfig {
            beam_width: 4,
            fuel: 100,
            timeout: default_timeout(),
        };
        let (result, stats) = bulb_search(&space, initial(), config, None);
        assert_eq!(result.path, vec![5, 3, 7]);
        assert_eq!(space.cost(&result), 15.0);
        assert_eq!(stats.evaluated_plans, 1);
    }

    #[test]
    fn backtracking_escapes_greedy_trap() {
        // Level 0 option 0 probes cheapest, but completing it incurs a
        // large penalty. Backtracking to option 1 finds a better plan.
        let level_costs: Vec<Vec<usize>> = vec![vec![1, 5], vec![2, 3]];
        let num_levels = level_costs.len();
        let costs_for_closure = level_costs.clone();
        let space = LevelSpace {
            options_per_level: level_costs
                .iter()
                .map(|costs| (0..costs.len()).collect())
                .collect(),
            cost_fn: Box::new(move |path, level| {
                let base: f64 = path
                    .iter()
                    .enumerate()
                    .map(|(lvl, &opt)| costs_for_closure[lvl][opt] as f64)
                    .sum();
                if path.first() == Some(&0) && level == num_levels {
                    base + 100.0
                } else {
                    base
                }
            }),
            effort: std::cell::Cell::new(0),
        };

        let (result, stats) = bulb_search(
            &space,
            initial(),
            BulbConfig {
                beam_width: 2,
                fuel: 10,
                timeout: default_timeout(),
            },
            None,
        );
        assert_eq!(result.path, vec![1, 0]);
        assert_eq!(space.cost(&result), 7.0);
        assert!(stats.evaluated_plans >= 2);
    }

    #[test]
    fn beam_diversity_rescues_dead_end() {
        // Option 0 probes cheap but completes at MAX; option 1 is
        // expensive but completes normally. Beam width 2 keeps both.
        let dead_end_space = || LevelSpace {
            options_per_level: vec![vec![0, 1], vec![0]],
            cost_fn: Box::new(|path, level| {
                let base: f64 = path.iter().map(|&p| if p == 0 { 1.0 } else { 5.0 }).sum();
                if level == 2 && path.first() == Some(&0) {
                    f64::MAX
                } else {
                    base
                }
            }),
            effort: std::cell::Cell::new(0),
        };

        let space = dead_end_space();
        let (result, stats) = bulb_search(
            &space,
            initial(),
            BulbConfig {
                beam_width: 2,
                fuel: 100,
                timeout: default_timeout(),
            },
            None,
        );
        assert_eq!(result.path[0], 1);
        assert!(space.cost(&result) < f64::MAX);
        assert!(stats.evaluated_plans >= 2);
    }

    /// The optimal path lives beyond the first beam slice. Discrepancy
    /// iterations reach it by spending a discrepancy to enter slice 2.
    #[test_log::test]
    fn discrepancy_reaches_beyond_beam_width() {
        let space = LevelSpace {
            options_per_level: vec![vec![1, 2, 3, 4, 5, 6], vec![0, 1], vec![0, 1]],
            cost_fn: Box::new(|path, level| {
                let base: f64 = path.iter().map(|&v| v as f64).sum();
                if level == 3 && path.first().is_some_and(|&v| v <= 4) {
                    base + 1000.0
                } else {
                    base
                }
            }),
            effort: std::cell::Cell::new(0),
        };

        let (greedy, _) = bulb_search(
            &space,
            initial(),
            BulbConfig {
                beam_width: 2,
                fuel: 0,
                timeout: default_timeout(),
            },
            None,
        );
        assert!(
            space.cost(&greedy) >= 1000.0,
            "greedy should find penalized path, got {}",
            space.cost(&greedy),
        );

        let (result, stats) = bulb_search(
            &space,
            initial(),
            BulbConfig {
                beam_width: 2,
                fuel: 10_000,
                timeout: default_timeout(),
            },
            None,
        );
        assert_eq!(
            result.path[0], 5,
            "should pick option 5 from slice 2, got {:?}",
            result.path,
        );
        assert_eq!(space.cost(&result), 5.0);
        assert!(
            stats.evaluated_plans <= 10,
            "should not waste fuel on redundant completions, used {}",
            stats.evaluated_plans,
        );
    }

    #[test]
    fn fuel_bounds_search_effort() {
        let space = sum_space(vec![
            vec![1, 2, 3, 4, 5],
            vec![1, 2, 3, 4, 5],
            vec![1, 2, 3, 4, 5],
            vec![1, 2, 3, 4, 5],
        ]);
        let config = BulbConfig {
            beam_width: 5,
            fuel: 3,
            timeout: default_timeout(),
        };
        let (result, stats) = bulb_search(&space, initial(), config, None);
        assert_eq!(result.level, 4);
        assert!(stats.evaluated_plans <= 3);
    }
}
