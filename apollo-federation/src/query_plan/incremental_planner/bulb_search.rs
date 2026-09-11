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
/// Backtracking; Furcy and Koenig 2005, "Limited Discrepancy Beam Search").
///
/// Operates on a single mutable candidate via checkpoint/rollback instead
/// of cloning; `snapshot()` is called only to save the best complete candidate.
///
/// # Example
///
/// A two-decision space where the option that probes cheapest at the
/// first decision forces an expensive follow-up. The greedy pass (fuel 0)
/// falls into the trap; one discrepancy iteration escapes it.
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
/// assert_eq!(greedy.unwrap(), vec![0, 10]);
///
/// // With fuel, a discrepancy iteration revisits the first decision
/// // and finds the cheaper plan.
/// let space = Trap { effort: Cell::new(0) };
/// let (best, _) = bulb_search(&space, vec![], config(100), None);
/// assert_eq!(best.unwrap(), vec![1, 0]);
/// ```
pub trait BulbSearchSpace {
    type Candidate;
    type Decision: Clone;
    type Choice: Clone;
    type Checkpoint: Clone;

    /// Advance past all deterministic (single-option) decisions in place,
    /// returning the next multi-option decision point or `Complete`.
    fn advance(&self, candidate: &mut Self::Candidate) -> AdvanceResult<Self::Decision>;

    /// Enumerate the choices available at a decision point. The search
    /// orders them by cost itself; enumeration order only breaks cost ties.
    fn options(&self, decision: &Self::Decision) -> Vec<Self::Choice>;

    /// Commit a choice in place. Only called with the candidate in the
    /// state where `advance` returned this decision.
    fn apply(
        &self,
        candidate: &mut Self::Candidate,
        decision: &Self::Decision,
        choice: &Self::Choice,
    );

    /// Save the candidate's current state for later rollback. O(1).
    fn checkpoint(&self, candidate: &Self::Candidate) -> Self::Checkpoint;

    /// Restore a previously saved checkpoint, undoing all mutations since.
    /// Checkpoints must be used in LIFO order. The effort counter (see
    /// [`effort`](Self::effort)) is exempt: it must keep counting
    /// rolled-back work, so store it outside the rolled-back state.
    fn rollback(&self, candidate: &mut Self::Candidate, cp: Self::Checkpoint);

    /// Full deep clone; used only to save the best complete candidate.
    fn snapshot(&self, candidate: &Self::Candidate) -> Self::Candidate;

    /// Heuristic cost of a (possibly partial) candidate. Lower is better.
    /// Must be monotonically non-decreasing as choices are applied, since
    /// the search prunes partial candidates against the best complete cost.
    fn cost(&self, candidate: &Self::Candidate) -> f64;

    /// Whether a terminal candidate satisfies the full request. Dead ends
    /// are never returned and never constrain the search. The default
    /// (always true) suits spaces where every terminal state is a solution.
    fn is_complete(&self, candidate: &Self::Candidate) -> bool {
        let _ = candidate;
        true
    }

    /// Monotonic total work spent across the whole search, including
    /// rolled-back work; fuel (see [`BulbConfig::fuel`]) is measured in
    /// these units. The default (always 0) disables effort budgeting, so
    /// pair it with a timeout unless the space is finite: the search has
    /// no no-improvement cutoff.
    fn effort(&self, candidate: &Self::Candidate) -> u64 {
        let _ = candidate;
        0
    }
}

#[derive(Debug, Clone)]
pub struct BulbConfig {
    /// Options explored per decision. The greedy first iteration always
    /// uses 1; later iterations use this value.
    pub beam_width: usize,
    /// Cap on effort (see [`BulbSearchSpace::effort`]) spent after the
    /// first complete candidate; the search for that one runs unbudgeted.
    /// `fuel: 0` stops at the first complete candidate. Exhaustion returns
    /// the best found so far.
    pub fuel: u64,
    /// Optional wall-clock limit, after which the best solution so far is
    /// returned, making the result machine-load dependent. Leave `None`
    /// (the default) for deterministic, fuel-bounded search.
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

/// Why a BULB search stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulbTermination {
    /// The search explored all reachable discrepancy combinations.
    Exhaustive,
    /// The fuel budget was exhausted.
    FuelExhausted,
    /// The wall-clock timeout fired.
    TimedOut,
    /// Cooperative cancellation was requested.
    Cancelled,
}

/// Statistics from a BULB search run.
pub struct BulbStats {
    /// Terminal candidates evaluated, including incomplete ones.
    pub evaluated_plans: usize,
    /// Decision points expanded (advanced to and scored).
    pub expansions: usize,
    /// Total effort spent (see [`BulbSearchSpace::effort`]).
    pub effort: u64,
    /// Effort already spent when the first complete candidate was recorded;
    /// fuel consumption is measured from this point. `None` when no
    /// complete candidate was ever found.
    pub first_complete_effort: Option<u64>,
    /// Why the search terminated.
    pub termination: BulbTermination,
}

/// Run BULB search on the given search space, returning the best complete
/// candidate found (if any) along with statistics. `None` means no
/// candidate satisfying [`BulbSearchSpace::is_complete`] was ever reached.
///
/// A DFS variant of Furcy and Koenig 2005 "Limited Discrepancy Beam
/// Search": a single
/// mutable candidate with checkpoint/rollback instead of a beam of cloned
/// states. Iteration 0 is a greedy descent for a fast first plan; each
/// later iteration allows one more discrepancy (an option outside the
/// cheapest `beam_width` at one decision) and the best complete candidate
/// improves monotonically until exhaustion or a budget (fuel, timeout,
/// cancellation) stops the search.
pub fn bulb_search<S: BulbSearchSpace>(
    space: &S,
    mut initial: S::Candidate,
    config: BulbConfig,
    check_cancellation: Option<&dyn Fn() -> ControlFlow<()>>,
) -> (Option<S::Candidate>, BulbStats) {
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
        last_effort: 0,
        best: None,
        best_cost: f64::MAX,
    };
    let mut termination = BulbTermination::Exhaustive;

    let initial_cp = space.checkpoint(&initial);

    for max_disc in 0.. {
        // Divergence from Furcy and Koenig 2005, which uses B every iteration: force
        // B=1 on the greedy pass. Scoring B options per level allocates
        // B * depth without improving the first candidate, since greedy
        // only follows the best option anyway.
        let effective_b = if max_disc == 0 { 1 } else { b };
        trace!(
            max_disc,
            effective_b, progress.completions, "starting BULB probe iteration",
        );

        let alternatives_existed =
            bulb_probe(space, &mut initial, max_disc, effective_b, &mut progress);

        space.rollback(&mut initial, initial_cp.clone());

        trace!(
            max_disc,
            total_completions = progress.completions,
            alternatives_existed,
            progress.best_cost,
            "BULB probe iteration done",
        );

        if let Some(reason) = progress.exhausted(space.effort(&initial)) {
            termination = reason;
            debug!(
                max_disc,
                progress.completions,
                ?reason,
                "BULB search stopped"
            );
            break;
        }
        if !alternatives_existed {
            break;
        }
    }

    (
        progress.best,
        BulbStats {
            evaluated_plans: progress.completions,
            expansions: progress.expansions,
            effort: space.effort(&initial),
            first_complete_effort: progress.first_complete_effort,
            termination,
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
    /// Effort cap, armed at the first complete candidate (its effort plus
    /// `fuel`). `None` until then: fuel bounds optimization beyond a
    /// complete plan, never the search for one.
    effort_budget: Option<u64>,
    /// Effort at the moment the first complete candidate was recorded.
    first_complete_effort: Option<u64>,
    /// Last effort value observed, for monotonicity assertions.
    last_effort: u64,
    /// Best complete candidate found so far, never an incomplete one.
    best: Option<C>,
    /// Incumbent prune bound: cheapest complete candidate cost seen.
    /// Incomplete terminals never tighten it, so it never prunes a path
    /// to a reachable complete plan.
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

    fn exhausted(&mut self, effort: u64) -> Option<BulbTermination> {
        debug_assert!(
            effort >= self.last_effort,
            "effort must be monotonically non-decreasing: {} < {}",
            effort,
            self.last_effort,
        );
        self.last_effort = effort;
        if self.effort_budget.is_some_and(|budget| effort >= budget) {
            Some(BulbTermination::FuelExhausted)
        } else if self.out_of_time() {
            Some(BulbTermination::TimedOut)
        } else if self.cancelled() {
            Some(BulbTermination::Cancelled)
        } else {
            None
        }
    }
}

/// One decision point of the BULB DFS, on an explicit stack so deeply
/// nested queries don't overflow the call stack. Options are arranged in
/// exploration order at construction: alt slices first, then the best
/// slice, following the paper's backtrack-alternatives-before-greedy order.
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
        }
    }

    /// Next option to explore, with the discrepancy budget for its
    /// children. Options costing at least the incumbent are pruned; each
    /// section is sorted, so one pruned entry skips the section's rest.
    fn next_option(&mut self, best_cost: f64) -> Option<(usize, usize)> {
        while self.pos < self.order.len() {
            let (opt_idx, cost) = self.order[self.pos];
            if cost >= best_cost {
                if self.pos < self.alt_end {
                    self.pos = self.alt_end;
                } else {
                    break;
                }
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

/// Iterative DFS BULB probe. At each decision point, all options are
/// scored (apply -> cost -> rollback) and sorted; disc=0 explores the best
/// slice only, disc>0 explores alt slices first (disc-1), then the best
/// slice (full disc). Returns whether any options were dropped for lack
/// of discrepancy budget (a genuine alternative to backtrack into).
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
            if progress.exhausted(space.effort(candidate)).is_some() {
                break 'descend false;
            }

            match space.advance(candidate) {
                AdvanceResult::Complete => {
                    // Only cancellation skips recording. On fuel or time
                    // exhaustion the completion is already reached and the
                    // snapshot is cheap, so keep it.
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
                        progress.best_cost,
                    );

                    if progress.exhausted(space.effort(candidate)).is_some() || scored.is_empty() {
                        break 'descend false;
                    }

                    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
                    // Flag alternatives only when actually dropped. With
                    // disc > 0 the alt slices are visited in this probe,
                    // and anything unexplored beneath them gets flagged by
                    // a descendant at disc == 0.
                    result_alts |= disc_budget == 0 && scored.len() > beam_width;
                    let mut frame = BulbFrame::new(
                        decision,
                        options,
                        scored,
                        checkpoint,
                        disc_budget,
                        beam_width,
                    );

                    debug_assert!(
                        !frame.order.is_empty(),
                        "non-empty scored produced an empty exploration order",
                    );

                    if let Some((opt_idx, child_disc)) = frame.next_option(progress.best_cost) {
                        space.apply(candidate, &frame.decision, &frame.options[opt_idx]);
                        disc_budget = child_disc;
                        stack.push(frame);
                        true
                    } else {
                        false
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
            if progress.exhausted(space.effort(candidate)).is_none()
                && let Some((opt_idx, child_disc)) = frame.next_option(progress.best_cost)
            {
                disc_budget = child_disc;
                space.apply(candidate, &frame.decision, &frame.options[opt_idx]);
                continue 'search;
            }
            stack.pop();
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
    best_cost: f64,
) -> Vec<(usize, f64)> {
    let mut scored: Vec<(usize, f64)> = Vec::with_capacity(options.len());

    for (i, choice) in options.iter().enumerate() {
        space.apply(candidate, decision, choice);
        let cost = space.cost(candidate);
        if cost < best_cost {
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

/// Record a terminal candidate. Only complete candidates are saved and
/// tighten the prune bound, since admitting dead ends would incorrectly
/// prune paths to better complete plans. The first complete candidate
/// arms the fuel budget.
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
    if complete {
        if progress.first_complete_effort.is_none() {
            let effort = space.effort(candidate);
            progress.first_complete_effort = Some(effort);
            progress.effort_budget = Some(effort.saturating_add(progress.fuel));
        }
        if improved {
            progress.best_cost = cost;
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
        let result = result.expect("should find a complete plan");
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
        let result = result.expect("should find a complete plan");
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
        let result = result.expect("should find a complete plan");
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
        let (result, _stats) = bulb_search(
            &space,
            initial(),
            BulbConfig {
                beam_width: 2,
                fuel: 100,
                timeout: default_timeout(),
            },
            None,
        );
        let result = result.expect("should find a complete plan");
        assert_eq!(result.path[0], 1);
        assert!(space.cost(&result) < f64::MAX);
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
        let greedy = greedy.expect("greedy should find a plan");
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
        let result = result.expect("should find a complete plan");
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
        let result = result.expect("should find a complete plan");
        assert_eq!(result.level, 4);
        assert!(stats.evaluated_plans <= 3);
    }
}
