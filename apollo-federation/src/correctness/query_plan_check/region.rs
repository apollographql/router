//! Sets of configurations, written as unions of conjunctions of literals.
//!
//! A configuration gives every copy of the response a supergraph runtime type, or none where the
//! copy is absent, and every Boolean variable a value. A [`Literal`] constrains one variable: a
//! copy's runtime type to a set of supergraph objects, or a Boolean variable to a value. A
//! [`Clause`] is a conjunction with at most one literal per variable, sorted by variable, and a
//! [`Region`] is a union of clauses; an empty region holds no configuration and the empty clause
//! holds every one. Only the same copy shares a variable: copies at equal depth on different
//! paths are different variables.
//!
//! A set of runtime types is a bitset over the supergraph's objects, with one more bit for an
//! absent copy, interned as a [`TypeSet`] so that literals compare and hash as integers. A set is
//! `W` words wide, `W` the objects and the absent bit over 64. The set of every object and the set
//! holding the absent copy alone are built once, with the algebra; membership reads one word, and
//! a subset test reads words until one decides it, without building a set. A union, an
//! intersection or a difference is built: its `W` words computed and interned, the interner finding
//! an equal set by its words, hashed with a non-cryptographic hasher, so its cost is `W` words in
//! expectation, where the hasher spreads the sets: an expected cost, not a bound every input meets.
//!
//! [`Work`] counts the loops of the representation and the walks over it, for the growth laws:
//! each counter names the loop it counts.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;

use apollo_compiler::Name;
use apollo_compiler::schema::ExtendedType;

use super::super::query_compare::conditions::BooleanLiteral;
use super::response::CopyId;
use crate::schema::ValidFederationSchema;

//==================================================================================================
// Variables and literals
//==================================================================================================

/// What a literal constrains. `Local` is a node of a fetch's own additions before they are
/// mounted on the copies they reach (see `response.rs`), and never outlives the fetch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum Var {
    Copy(CopyId),
    Local(u32),
    Boolean(Name),
}

/// A set of runtime types, or a Boolean value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Value {
    Types(TypeSet),
    Is(bool),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Literal {
    pub(super) var: Var,
    pub(super) value: Value,
}

/// A conjunction of literals, sorted by variable, one per variable.
pub(super) type Clause = Vec<Literal>;

/// A union of clauses.
pub(super) type Region = Vec<Clause>;

/// The region holding every configuration.
pub(super) fn everything() -> Region {
    vec![Vec::new()]
}

/// A Boolean literal of the plan or an operation as a literal of a clause.
pub(super) fn boolean(literal: &BooleanLiteral) -> Literal {
    Literal {
        var: Var::Boolean(literal.variable().clone()),
        value: Value::Is(literal.required_value()),
    }
}

/// The variables a clause constrains.
pub(super) fn vars(clause: &Clause) -> impl Iterator<Item = &Var> {
    clause.iter().map(|literal| &literal.var)
}

//==================================================================================================
// Sets of runtime types
//==================================================================================================

/// An interned set of supergraph objects, the absent copy included as one more member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct TypeSet(u32);

impl TypeSet {
    pub(super) const EMPTY: TypeSet = TypeSet(0);
}

/// The interned sets: each one's words stored once, shared by its id's slot and the map finding it,
/// which compares a found set's words with the asked one's, never a digest of them.
struct SetTable {
    bits: Vec<Rc<[u64]>>,
    ids: HashMap<Rc<[u64]>, TypeSet, ahash::RandomState>,
}

/// The supergraph's objects, the interned sets of them, and the work counters.
pub(super) struct Algebra {
    objects: Vec<Name>,
    index: HashMap<Name, u32>,
    words: usize,
    sets: RefCell<SetTable>,
    /// Every object, the absent copy excluded, and the absent copy alone.
    every: TypeSet,
    absent: TypeSet,
    pub(super) work: Work,
}

impl Algebra {
    pub(super) fn new(supergraph: &ValidFederationSchema) -> Self {
        let objects: Vec<Name> = supergraph
            .schema()
            .types
            .iter()
            .filter(|(_, ty)| matches!(ty, ExtendedType::Object(_)))
            .map(|(name, _)| name.clone())
            .collect();
        let index = objects
            .iter()
            .enumerate()
            .map(|(bit, name)| (name.clone(), bit as u32))
            .collect();
        // One more bit for the absent copy.
        let words = (objects.len() + 1).div_ceil(64);
        let empty: Rc<[u64]> = Rc::from(vec![0; words]);
        let mut ids = HashMap::default();
        ids.insert(empty.clone(), TypeSet::EMPTY);
        let mut algebra = Algebra {
            index,
            words,
            sets: RefCell::new(SetTable {
                bits: vec![empty],
                ids,
            }),
            every: TypeSet::EMPTY,
            absent: TypeSet::EMPTY,
            work: Work::default(),
            objects,
        };
        let absent = algebra.objects.len();
        let mut every = vec![u64::MAX; absent / 64];
        every.extend((!absent.is_multiple_of(64)).then(|| (1u64 << (absent % 64)) - 1));
        every.resize(words, 0);
        let mut alone = vec![0u64; words];
        alone[absent / 64] = 1 << (absent % 64);
        algebra.every = algebra.intern(every.into_boxed_slice());
        algebra.absent = algebra.intern(alone.into_boxed_slice());
        algebra
    }

    fn intern(&self, bits: Box<[u64]>) -> TypeSet {
        self.work.interned_words.add(bits.len() as u64);
        let mut table = self.sets.borrow_mut();
        if let Some(set) = table.ids.get(&bits[..]) {
            return *set;
        }
        let set = TypeSet(table.bits.len() as u32);
        let bits: Rc<[u64]> = Rc::from(bits);
        table.bits.push(bits.clone());
        table.ids.insert(bits, set);
        self.work.type_sets.add(1);
        self.work.set_words_stored.add(self.words as u64);
        set
    }

    fn combine(&self, left: TypeSet, right: TypeSet, op: impl Fn(u64, u64) -> u64) -> TypeSet {
        self.work.set_words.add(self.words as u64);
        let bits: Box<[u64]> = {
            let table = self.sets.borrow();
            let (left, right) = (&table.bits[left.0 as usize], &table.bits[right.0 as usize]);
            left.iter()
                .zip(right.iter())
                .map(|(l, r)| op(*l, *r))
                .collect()
        };
        self.intern(bits)
    }

    pub(super) fn intersect(&self, left: TypeSet, right: TypeSet) -> TypeSet {
        if left == right || right == TypeSet::EMPTY {
            return right;
        }
        if left == TypeSet::EMPTY {
            return left;
        }
        self.combine(left, right, |l, r| l & r)
    }

    pub(super) fn union(&self, left: TypeSet, right: TypeSet) -> TypeSet {
        if left == right || right == TypeSet::EMPTY {
            return left;
        }
        if left == TypeSet::EMPTY {
            return right;
        }
        self.combine(left, right, |l, r| l | r)
    }

    /// The union of several sets, built in one pass over their words.
    pub(super) fn union_all(&self, sets: &[TypeSet]) -> TypeSet {
        match sets {
            [] => TypeSet::EMPTY,
            [one] => *one,
            _ => {
                let bits: Box<[u64]> = {
                    let table = self.sets.borrow();
                    let mut words = vec![0u64; self.words];
                    for set in sets {
                        self.work.set_words.add(self.words as u64);
                        self.work.joined_words.add(self.words as u64);
                        for (word, own) in words.iter_mut().zip(table.bits[set.0 as usize].iter()) {
                            *word |= own;
                        }
                    }
                    words.into_boxed_slice()
                };
                self.intern(bits)
            }
        }
    }

    pub(super) fn minus(&self, left: TypeSet, right: TypeSet) -> TypeSet {
        if left == right {
            return TypeSet::EMPTY;
        }
        if left == TypeSet::EMPTY || right == TypeSet::EMPTY {
            return left;
        }
        self.combine(left, right, |l, r| l & !r)
    }

    /// Does `left` hold no member `right` lacks? Read word by word until a word decides it: all
    /// `W` words where it does.
    pub(super) fn is_subset(&self, left: TypeSet, right: TypeSet) -> bool {
        if left == right || left == TypeSet::EMPTY {
            return true;
        }
        let table = self.sets.borrow();
        let (left, right) = (&table.bits[left.0 as usize], &table.bits[right.0 as usize]);
        for (l, r) in left.iter().zip(right.iter()) {
            self.work.set_words.add(1);
            if l & !r != 0 {
                return false;
            }
        }
        true
    }

    /// The set of these members, as [`Self::bits`] gives them: `W` words built and each member
    /// visited once.
    pub(super) fn of_bits(&self, bits: impl IntoIterator<Item = u32>) -> TypeSet {
        let mut words = vec![0u64; self.words];
        for bit in bits {
            self.work.set_bits.add(1);
            words[(bit / 64) as usize] |= 1 << (bit % 64);
        }
        self.intern(words.into_boxed_slice())
    }

    /// The supergraph objects of these names; a name that is not an object adds nothing.
    pub(super) fn objects<'n>(&self, names: impl IntoIterator<Item = &'n Name>) -> TypeSet {
        self.of_bits(
            names
                .into_iter()
                .filter_map(|name| self.index.get(name).copied()),
        )
    }

    /// The set holding the absent copy alone.
    pub(super) fn absent(&self) -> TypeSet {
        self.absent
    }

    /// Every supergraph object, the absent copy excluded.
    pub(super) fn every_object(&self) -> TypeSet {
        self.every
    }

    /// The names of a set's objects, in the supergraph's order; the absent copy has none.
    pub(super) fn names(&self, set: TypeSet) -> Vec<Name> {
        self.bits(set)
            .into_iter()
            .filter_map(|bit| self.objects.get(bit as usize).cloned())
            .collect()
    }

    /// The members of a set as bits, the absent copy's last.
    pub(super) fn bits(&self, set: TypeSet) -> Vec<u32> {
        let table = self.sets.borrow();
        let words = &table.bits[set.0 as usize];
        self.work.set_words.add(self.words as u64);
        let mut bits = Vec::new();
        for (index, word) in words.iter().enumerate() {
            let mut word = *word;
            while word != 0 {
                let bit = word.trailing_zeros();
                bits.push(index as u32 * 64 + bit);
                word &= word - 1;
            }
        }
        bits
    }

    /// Whether a set holds the object of this name: one word read.
    pub(super) fn contains(&self, set: TypeSet, name: &Name) -> bool {
        self.index.get(name).is_some_and(|bit| {
            self.work.set_words.add(1);
            let table = self.sets.borrow();
            table.bits[set.0 as usize][(bit / 64) as usize] & (1 << (bit % 64)) != 0
        })
    }

    /// A set as an explanation writes it.
    pub(super) fn render(&self, set: TypeSet) -> String {
        let mut names: Vec<String> = self
            .bits(set)
            .into_iter()
            .map(|bit| match self.objects.get(bit as usize) {
                Some(name) => format!("`{name}`"),
                None => "absent".to_string(),
            })
            .collect();
        names.sort();
        names.join(" or ")
    }
}

//==================================================================================================
// Clauses and regions
//==================================================================================================

/// The values a variable can take, for completing a literal into its complement: a copy's
/// possible runtime types where a walk knows them, every object otherwise.
pub(super) type Domain<'d> = &'d dyn Fn(&Var) -> TypeSet;

impl Algebra {
    fn and_value(&self, left: Value, right: Value) -> Option<Value> {
        match (left, right) {
            (Value::Types(left), Value::Types(right)) => {
                let both = self.intersect(left, right);
                (both != TypeSet::EMPTY).then_some(Value::Types(both))
            }
            (Value::Is(left), Value::Is(right)) => (left == right).then_some(Value::Is(left)),
            _ => None,
        }
    }

    /// Two conjunctions as one, `None` when no configuration satisfies both.
    pub(super) fn conjoin(&self, left: &[Literal], right: &[Literal]) -> Option<Clause> {
        let mut out = Vec::with_capacity(left.len() + right.len());
        let (mut i, mut j) = (0, 0);
        while i < left.len() && j < right.len() {
            self.work.literals.add(1);
            match left[i].var.cmp(&right[j].var) {
                std::cmp::Ordering::Less => {
                    out.push(left[i].clone());
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    out.push(right[j].clone());
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    let Some(value) = self.and_value(left[i].value, right[j].value) else {
                        self.work.clauses_contradictory.add(1);
                        return None;
                    };
                    out.push(Literal {
                        var: left[i].var.clone(),
                        value,
                    });
                    i += 1;
                    j += 1;
                }
            }
        }
        out.extend_from_slice(&left[i..]);
        out.extend_from_slice(&right[j..]);
        self.work.clauses.add(1);
        Some(out)
    }

    /// A clause from literals in any order, `None` when they contradict each other.
    pub(super) fn clause(&self, literals: impl IntoIterator<Item = Literal>) -> Option<Clause> {
        let mut clause = Vec::new();
        for literal in literals {
            clause = self.conjoin(&clause, std::slice::from_ref(&literal))?;
        }
        Some(clause)
    }

    /// Every pairing of a clause of one region with a clause of the other.
    pub(super) fn product(&self, left: &[Clause], right: &[Clause]) -> Region {
        let mut out = Vec::new();
        for l in left {
            for r in right {
                if let Some(both) = self.conjoin(l, r) {
                    out.push(both);
                }
            }
        }
        self.made(&out);
        out
    }

    /// Records the size of a region where it is made.
    fn made(&self, region: &[Clause]) {
        self.work.largest_region_clauses.max(region.len() as u64);
        self.work
            .largest_region_literals
            .max(region.iter().map(|clause| clause.len() as u64).sum());
    }

    /// The configurations of `clause` that `cover` does not hold, as disjoint clauses.
    fn subtract(&self, clause: &Clause, cover: &Clause, domain: Domain<'_>) -> Region {
        let mut current = clause.clone();
        let mut out = Vec::new();
        for literal in cover {
            self.work.literals.add(1);
            match current.iter().position(|own| own.var == literal.var) {
                Some(at) => {
                    let own = current[at].value;
                    let nonempty = |set: TypeSet| (set != TypeSet::EMPTY).then_some(set);
                    let (inside, outside) = match (own, literal.value) {
                        (Value::Types(own), Value::Types(cover)) => (
                            nonempty(self.intersect(own, cover)).map(Value::Types),
                            nonempty(self.minus(own, cover)).map(Value::Types),
                        ),
                        (Value::Is(own), Value::Is(cover)) if own == cover => {
                            (Some(Value::Is(own)), None)
                        }
                        _ => (None, Some(own)),
                    };
                    let Some(inside) = inside else {
                        out.push(current);
                        return out;
                    };
                    if let Some(outside) = outside {
                        let mut rest = current.clone();
                        rest[at].value = outside;
                        out.push(rest);
                    }
                    current[at].value = inside;
                }
                None => {
                    let complement = match literal.value {
                        Value::Types(cover) => {
                            let rest = self.minus(domain(&literal.var), cover);
                            (rest != TypeSet::EMPTY).then_some(Value::Types(rest))
                        }
                        Value::Is(cover) => Some(Value::Is(!cover)),
                    };
                    if let Some(complement) = complement {
                        let rest = self.conjoin(
                            &current,
                            &[Literal {
                                var: literal.var.clone(),
                                value: complement,
                            }],
                        );
                        out.extend(rest);
                    }
                    current = match self.conjoin(&current, std::slice::from_ref(literal)) {
                        Some(current) => current,
                        None => return out,
                    };
                }
            }
        }
        self.work.clauses_covered.add(1);
        out
    }

    /// The configurations of `region` no clause of `covers` holds.
    pub(super) fn subtract_all(
        &self,
        region: Region,
        covers: &[Clause],
        domain: Domain<'_>,
    ) -> Region {
        let mut remaining = region;
        for cover in covers {
            remaining = remaining
                .iter()
                .flat_map(|clause| self.subtract(clause, cover, domain))
                .collect();
            self.work.residual_clauses.add(remaining.len() as u64);
            self.made(&remaining);
            if remaining.is_empty() {
                break;
            }
        }
        remaining
    }

    /// Existential projection of one clause onto the variables `keep` admits: dropping a literal
    /// is exact because a clause's literals are each satisfiable and constrain separate variables.
    pub(super) fn retain(&self, clause: Clause, keep: impl Fn(&Var) -> bool) -> Clause {
        self.work.literals.add(clause.len() as u64);
        clause
            .into_iter()
            .filter(|literal| keep(&literal.var))
            .collect()
    }

    /// Does `narrower` imply `wider`: every literal of `wider` has one on its variable in
    /// `narrower` whose values it holds? Both are sorted by variable, so the two are walked
    /// together, each literal visited once.
    fn implies(&self, narrower: &Clause, wider: &Clause) -> bool {
        let mut own = narrower.iter();
        'wider: for literal in wider {
            self.work.literals.add(1);
            for candidate in own.by_ref() {
                self.work.literals.add(1);
                match candidate.var.cmp(&literal.var) {
                    std::cmp::Ordering::Less => continue,
                    std::cmp::Ordering::Greater => return false,
                    std::cmp::Ordering::Equal => {
                        let holds = match (candidate.value, literal.value) {
                            (Value::Types(own), Value::Types(cover)) => self.is_subset(own, cover),
                            (own, cover) => own == cover,
                        };
                        if holds {
                            continue 'wider;
                        }
                        return false;
                    }
                }
            }
            return false;
        }
        true
    }

    /// The same configurations in fewer clauses. A literal holding every value its variable can
    /// take is dropped. Then, for each variable in order, the clauses equal but for their literal
    /// on it are joined into one holding the values of all of them, found by bucketing each clause
    /// on its other literals: one pass per variable, which keeps the configurations but need not
    /// reach the fewest clauses joining could, since a join on a later variable can make clauses
    /// joinable on an earlier one. Last, a clause another implies is dropped, the most general
    /// clauses first: each clause is compared with the clauses kept before it, in order, up to the
    /// first implying it.
    ///
    /// The work, for a region of `L` literals over `V` variables:
    /// - the index of the clauses holding each variable, ordered by variable: every literal read
    ///   once and placed among the variables in `O(log V)` comparisons, `O(L log V)` in all, and
    ///   one entry stored per literal;
    /// - the bucket keys' literals, built, hashed and compared: a clause is bucketed once per
    ///   variable it holds, on a key of its other literals, so a clause costs its width squared
    ///   and the keys the region's literals times its widest clause's, in expectation, where the
    ///   hasher spreads the keys: not a bound every input meets. A pass holds its keys, an entry per
    ///   bucket and the values joined into its buckets, one per clause holding its variable, and
    ///   drops them when it ends;
    /// - the words joined, each joined value's `W` once;
    /// - absorption's visits, each one comparison with the literals and words it reads: a clause
    ///   visits the kept clauses in order and stops at the first implying it, so it makes at most
    ///   as many as the clauses kept before it, and absorption is quadratic in the clauses left
    ///   after joining, which is all of them where no two join, as for `x = i ∧ y = i` over `i`.
    pub(super) fn simplify(&self, region: Region, domain: Domain<'_>) -> Region {
        self.made(&region);
        if region.len() < 2 {
            let region: Region = region
                .into_iter()
                .map(|clause| self.without_full_literals(clause, domain))
                .collect();
            self.work.region_clauses_max.max(region.len() as u64);
            return region;
        }
        let mut clauses: Vec<Option<Clause>> = region
            .into_iter()
            .map(|clause| Some(self.without_full_literals(clause, domain)))
            .collect();
        // Each variable, in order, with the clauses holding a literal on it, in theirs.
        let mut holders: BTreeMap<Var, Vec<usize>> = BTreeMap::new();
        for (index, clause) in clauses.iter().enumerate() {
            for literal in clause.iter().flatten() {
                self.work.literals.add(1);
                holders.entry(literal.var.clone()).or_default().push(index);
            }
        }
        for (var, holding) in &holders {
            if holding.len() > 1 {
                self.join_on(&mut clauses, var, holding, domain);
            }
        }
        // The most general first: a clause another implies is then compared with the few kept
        // before it rather than kept beside them.
        let mut clauses: Region = clauses.into_iter().flatten().collect();
        clauses.sort_by_key(|clause| clause.len());
        let mut kept: Region = Vec::with_capacity(clauses.len());
        for clause in clauses {
            if !self.absorbed(&mut kept, &clause) {
                kept.push(clause);
            }
        }
        self.work.region_clauses_max.max(kept.len() as u64);
        kept
    }

    /// Is `clause` implied by a kept clause? Each kept clause is visited in order and compared
    /// with `clause`, up to the first implying it; a kept clause `clause` implies is dropped, the
    /// others keeping their order. No kept clause implies another, and implication is transitive,
    /// so a clause found implied has dropped none, and the kept clauses after the one implying it
    /// are not visited: nothing there would change.
    fn absorbed(&self, kept: &mut Region, clause: &Clause) -> bool {
        let mut retained = 0;
        for at in 0..kept.len() {
            self.work.absorption_visits.add(1);
            self.work.clause_comparisons.add(1);
            if self.implies(clause, &kept[at]) {
                // Nothing dropped, as said above; were there anything, it would stay dropped.
                kept.drain(retained..at);
                return true;
            }
            if !self.implies(&kept[at], clause) {
                self.work.clause_comparisons_unsuccessful.add(1);
                kept.swap(retained, at);
                retained += 1;
            }
        }
        kept.truncate(retained);
        false
    }

    /// The clauses `holding` a literal on `var` that are equal but for it, joined: each bucketed
    /// on its other literals, in the order the clauses come, the first of a bucket keeping the
    /// values of all of them, joined in one pass, and the literal dropped where they hold every
    /// value the variable can take; the others removed.
    fn join_on(
        &self,
        clauses: &mut [Option<Clause>],
        var: &Var,
        holding: &[usize],
        domain: Domain<'_>,
    ) {
        // Each bucket's first clause, with the values the others joined into it.
        let mut opened: Vec<(usize, Vec<Value>)> = Vec::new();
        let mut buckets: HashMap<Clause, usize, ahash::RandomState> = HashMap::default();
        for &index in holding {
            let Some(clause) = &clauses[index] else {
                continue;
            };
            let Ok(at) = clause.binary_search_by(|literal| {
                self.work.literals.add(1);
                literal.var.cmp(var)
            }) else {
                continue;
            };
            self.work.clause_comparisons.add(1);
            let mut key = clause.clone();
            let value = key.remove(at).value;
            self.work.bucket_literals.add(key.len() as u64);
            match buckets.get(&key) {
                Some(&bucket) => {
                    self.work.bucket_literals.add(key.len() as u64);
                    self.work.clauses_merged.add(1);
                    opened[bucket].1.push(value);
                    clauses[index] = None;
                }
                None => {
                    self.work.bucket_storage.add(key.len() as u64 + 1);
                    buckets.insert(key, opened.len());
                    opened.push((index, Vec::new()));
                }
            }
        }
        for (index, values) in opened {
            if values.is_empty() {
                continue;
            }
            let Some(clause) = &mut clauses[index] else {
                continue;
            };
            let Ok(at) = clause.binary_search_by(|literal| literal.var.cmp(var)) else {
                continue;
            };
            let value = match clause[at].value {
                Value::Types(first) => {
                    let mut sets = vec![first];
                    sets.extend(values.into_iter().filter_map(|value| match value {
                        Value::Types(types) => Some(types),
                        Value::Is(_) => None,
                    }));
                    let types = self.union_all(&sets);
                    (!self.is_subset(domain(var), types)).then_some(Value::Types(types))
                }
                Value::Is(first) => values
                    .iter()
                    .all(|value| *value == Value::Is(first))
                    .then_some(Value::Is(first)),
            };
            match value {
                Some(value) => clause[at].value = value,
                None => {
                    clause.remove(at);
                }
            }
        }
    }

    fn without_full_literals(&self, clause: Clause, domain: Domain<'_>) -> Clause {
        clause
            .into_iter()
            .filter(|literal| match (&literal.var, literal.value) {
                (Var::Boolean(_), _) => true,
                (var, Value::Types(set)) => !self.is_subset(domain(var), set),
                _ => true,
            })
            .collect()
    }
}

//==================================================================================================
// Work
//==================================================================================================

/// One counter per loop the growth laws bound. Each is incremented where its loop turns, rejected
/// probes included, so a law reads the work it bounds rather than what survived it.
#[derive(Default)]
pub(super) struct Work {
    // Construction.
    /// Selections visited while producing a fetch's additions, once per local node they are
    /// produced at.
    pub(super) selections_visited: Counter,
    /// Contribution records made, duplicates included.
    pub(super) contributions_recorded: Counter,
    /// Records dropped as identical to one the same fetch had already added.
    pub(super) contributions_deduplicated: Counter,
    /// Lookups of a copy by parent copy and response key.
    pub(super) copy_probes: Counter,
    /// Copies created.
    pub(super) copies: Counter,
    /// Contribution records an output rename duplicated or split.
    pub(super) rename_records: Counter,
    /// Provider parents examined for the sharing clause.
    pub(super) sharing_parents: Counter,
    /// Per subgraph, the candidates examined as standing for a supergraph object a contribution
    /// is read at, its own name and each interface it implements, rejected ones included; and the
    /// correspondences kept.
    pub(super) sharing_candidates: Counter,
    pub(super) sharing_entries: Counter,
    /// Names read establishing which subgraph objects stand for which supergraph objects: the
    /// supergraph objects an enumeration of one subgraph object's correspondence copies, and the
    /// one name a test of one correspondence reads.
    pub(super) correspondence_names: Counter,
    // Paths.
    /// Path elements followed.
    pub(super) path_steps: Counter,
    /// Contributions read while following a path or building a guard from one.
    pub(super) path_contributions: Counter,
    // Sets and clauses.
    /// Distinct type sets interned, and the words they store, each set's once.
    pub(super) type_sets: Counter,
    pub(super) set_words_stored: Counter,
    /// The words hashed interning a set, found or new.
    pub(super) interned_words: Counter,
    /// Bitset words read by set operations: each operand's `W` where a set is built, a predicate's
    /// up to the word that decides it.
    pub(super) set_words: Counter,
    /// Members visited building a set from them.
    pub(super) set_bits: Counter,
    /// Literals read by conjunction, subtraction, projection and comparison, by simplification's
    /// index of the clauses holding each variable, and by a pass searching each clause for its
    /// variable.
    pub(super) literals: Counter,
    /// Clauses made by conjunction.
    pub(super) clauses: Counter,
    /// Conjunctions found contradictory.
    pub(super) clauses_contradictory: Counter,
    /// Subtractions that left nothing of a clause.
    pub(super) clauses_covered: Counter,
    /// Clauses left after each subtraction of one cover from a region.
    pub(super) residual_clauses: Counter,
    /// Simplification's probes: a clause looked up in a pass's buckets, and a pair of clauses
    /// compared for absorption.
    pub(super) clause_comparisons: Counter,
    /// Of the pairs, those where neither absorbed the other.
    pub(super) clause_comparisons_unsuccessful: Counter,
    /// Kept clauses absorption visited, each compared with the clause visiting it: a clause's
    /// visits end at the kept clause implying it.
    pub(super) absorption_visits: Counter,
    /// Clauses joined into a bucket another clause opened.
    pub(super) clauses_merged: Counter,
    /// The literals of the bucket keys built and hashed, and compared again where a probe found its
    /// bucket; and those stored, one bucket's key and an entry per bucket, summed over the passes:
    /// what the passes retained in all, not the most held at once, each pass dropping its buckets
    /// when it ends.
    pub(super) bucket_literals: Counter,
    pub(super) bucket_storage: Counter,
    /// The words of the sets joined in one pass.
    pub(super) joined_words: Counter,
    /// The most clauses a simplified region held.
    pub(super) region_clauses_max: Counter,
    /// The most clauses, and the most literals, one region held where it was made: a product,
    /// each residual a subtraction leaves, a region given to be simplified. One region's size,
    /// not the storage alive at once: that is what the stored accounts count, with the regions an
    /// operation holds while it runs, a subtraction's base and covers and the residual it
    /// consumes beside the one it builds.
    pub(super) largest_region_clauses: Counter,
    pub(super) largest_region_literals: Counter,
    // Checking.
    /// Copies a coverage or matching walk visited.
    pub(super) copies_visited: Counter,
    /// Obligations decided: a residual subtraction or an emptiness test under the hypothesis.
    pub(super) obligations: Counter,
    /// Copies whose admissible runtime types were built, per walk.
    pub(super) admissibility: Counter,
    /// Contributions read while building them, once per runtime type of the copy.
    pub(super) admissibility_reads: Counter,
    /// Groups of runtime types with one admissibility region.
    pub(super) admissibility_groups: Counter,
    /// Projections of the hypothesis computed, and found in the walk's memo.
    pub(super) projections: Counter,
    pub(super) projection_hits: Counter,
    /// Variables kept by projections, summed and at most; the support each obligation carries.
    pub(super) projection_support: Counter,
    pub(super) projection_support_max: Counter,
    /// The variables hashed looking a projection up, found or computed.
    pub(super) projection_key_width: Counter,
    /// The literals read discovering an obligation's or a projection's variables.
    pub(super) support_literals: Counter,
    /// Clauses of computed projections, summed.
    pub(super) projection_clauses: Counter,
    /// Demand entries made, and merged into one with the same selections.
    pub(super) demand_entries: Counter,
    pub(super) demand_merges: Counter,
    /// The widths hashed finding a demand's entry: its selections, and its region's literals.
    pub(super) demand_key_width: Counter,
    /// Demanded fields compared with the calls of their key, and fields of different calls
    /// compared for being active together.
    pub(super) call_comparisons: Counter,
    // Storage.
    /// Contributions stored, and the literals of their guards.
    pub(super) contributions: Counter,
    pub(super) guard_literals: Counter,
}

/// A count, or a maximum.
#[derive(Default)]
pub(super) struct Counter(Cell<u64>);

impl Counter {
    pub(super) fn add(&self, n: u64) {
        self.0.set(self.0.get() + n);
    }

    pub(super) fn max(&self, n: u64) {
        if n > self.0.get() {
            self.0.set(n);
        }
    }

    #[cfg(test)]
    pub(super) fn get(&self) -> u64 {
        self.0.get()
    }
}
