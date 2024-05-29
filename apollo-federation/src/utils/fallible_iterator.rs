#![deny(rustdoc::broken_intra_doc_links)]

use itertools::Itertools;

#[allow(dead_code, unused)]
fn sandbox() {
    fn is_prime(i: usize) -> Result<bool, ()> {
        match i {
            0 | 1 => Err(()), // 0 and 1 are neither prime or composite
            2 | 3 => Ok(true),
            _ => Ok(false), // Every other number is composite, I guess
        }
    }

    let vals = (1..6).fallible_filter(|i| is_prime(*i));
    itertools::assert_equal(vals, vec![Err(()), Ok(2), Ok(3)]);
}

/// An extension trait for `Iterator`, similar to `Itertools`, that seeks to improve the ergonomics
/// around fallible operations.
///
/// "Fallible" here has a few different meanings. The operation that you are performing (such as a
/// `filter`) might yield a `Result` containing the value you actually want/need, but fallible can
/// also refer to the stream of items that you're iterating over (or both!). As much as possible, I
/// will use the following naming scheme in order to keep these ideas consistent:
///  - If the iterator yeilds an arbitary `T` and the operation that you wish to apply is of the
/// form `T -> Result`, then it will named `fallible_*`.
///  - If the iterator yeilds `Result<T>` and the operation is of the form `T -> U` (for arbitary
///  `U`), then it will named `*_ok`.
///  - If both iterator and operation yield `Result`, then it will named `and_then_*` (more on that
///  fewer down).
///
/// The first category mostly describes combinators that take closures that need specific types,
/// such as `filter` and things in the `any`/`all`/`find`/`fold` family. There are several
/// expirement features in `std` that offer similar functionalities.
///
/// The second category is mostly taken care of by `Itertools`. While they are not currently
/// implemented here (or in `Itertools`), this category would also contain methods like `*_err`
/// in addition to the "ok" methods.
///
/// The third category is the hardest to pin down. There are a ton of ways that you can combine two
/// results (just look at the docs page for `Result`), but, in general, the most common use case
/// that needs to be captured is the use of the try operator. For example, if you have a check that
/// is fallible, you likely will write that code like so:
/// ```no_test
/// for val in things {
///   let val = fallible_transformation(val)?;
///   if fallible_check(&val)? { continue }
///   process_value(val);
/// }
/// ```
/// In such a case, `process_value` is called if and only if both the transformation and check
/// return `Ok`. This is why methods in this category are named `and_then_*`.
///
/// There are, of course, methods that fall out of this taxonimy, but this covers the broad
/// strokes.
///
/// In all cases, errors are passed along until they are processed in some way. This includes
/// `Result::collect`, but also includes things like `Itertools::process_results` and things like
/// the `find` family.
///
/// Lastly, if you come across something that fits what this trait is trying to do and you have a
/// usecase for but that is not served by already, feel free to expand the functionalities!
// TODO: In std, methods like `all` and `any` are actually just specializations of `try_fold` using
// bools and `FlowControl`. When initially writting this, I, @TylerBloom, didn't take the time to
// write equalivalent folding methods. Should they be implemented in the future, we should rework
// existing methods to use them.
pub trait FallibleIterator: Sized + Itertools {
    /// The method transforms the current iterator, which yields `T`s, into an iterator that yields
    /// `Result<T, E>`. The predicate that is provided is fallible. If the predicate yields
    /// `Ok(false)`, the item is skipped. If the predicate yields `Err(e)`, that `T` is discard and
    /// iterator will yield `Err(e)` in its place. Lastly, if the predicate yields `Ok(true)`, the
    /// iterator will yield `Ok(val)`
    ///
    /// DOCS
    ///
    /// ```rust
    /// use apollo_federation::utils::FallibleIterator;
    ///
    /// // A totally accurate prime checker
    /// fn is_prime(i: usize) -> Result<bool, ()> {
    ///   match i {
    ///     0 | 1 => Err(()), // 0 and 1 are neither prime or composite
    ///     2 | 3 => Ok(true),
    ///     _ => Ok(false), // Every other number is composite, I guess
    ///   }
    /// }
    ///
    /// let vals = (1..6).fallible_filter(|i| is_prime(*i));
    /// itertools::assert_equal(vals, vec![Err(()), Ok(2), Ok(3)]);
    /// ```
    fn fallible_filter<F, E>(self, predicate: F) -> FallibleFilter<Self, F>
    where
        F: FnMut(&Self::Item) -> Result<bool, E>,
    {
        FallibleFilter {
            iter: self,
            predicate,
        }
    }

    // NOTE: There is a `filter_ok` method on `Itertools`, but there is not a `filter_err`. That
    // might be useful at some point.

    /// ```rust
    /// use apollo_federation::utils::FallibleIterator;
    ///
    /// // A totally accurate prime checker
    /// fn is_prime(i: usize) -> Result<bool, ()> {
    ///   match i {
    ///     0 | 1 => Err(()), // 0 and 1 are neither prime or composite
    ///     2 | 3 => Ok(true),
    ///     _ => Ok(false), // Every other number is composite, I guess
    ///   }
    /// }
    ///
    /// let vals = vec![Ok(0), Err(()), Err(()), Ok(3), Ok(4)].and_then_filter(|i| is_prime(*i));
    /// itertools::assert_equal(vals, vec![Err(()), Err(()), Err(()), Ok(3)]);
    /// ```
    // TODO(@TylerBloom): Write an example (or two) and rewrite the docs.
    fn and_then_filter<T, E, F>(self, predicate: F) -> AndThenFilter<Self, F>
    where
        Self: Iterator<Item = Result<T, E>>,
        F: FnMut(&T) -> Result<bool, E>,
    {
        AndThenFilter {
            iter: self,
            predicate,
        }
    }

    /// ```rust
    /// use apollo_federation::utils::FallibleIterator;
    ///
    /// // A totally accurate prime checker
    /// fn is_prime(i: usize) -> Result<bool, ()> {
    ///   match i {
    ///     0 | 1 => Err(()), // 0 and 1 are neither prime or composite
    ///     2 | 3 => Ok(true),
    ///     _ => Ok(false), // Every other number is composite, I guess
    ///   }
    /// }
    ///
    /// assert_eq!(Ok(true), [].into_iter().fallible_all(is_prime));
    /// assert_eq!(Ok(true), (2..4).fallible_all(is_prime));
    /// assert_eq!(Err(()), (1..4).fallible_all(is_prime));
    /// assert_eq!(Ok(false), (2..5).fallible_all(is_prime));
    /// assert_eq!(Err(()), (1..5).fallible_all(is_prime));
    /// ```
    // TODO(@TylerBloom): Write an example (or two) and write the docs.
    fn fallible_all<E, F>(&mut self, mut predicate: F) -> Result<bool, E>
    where
        F: FnMut(Self::Item) -> Result<bool, E>,
    {
        let mut digest = true;
        for val in self.by_ref() {
            digest &= predicate(val)?;
            if !digest {
                break;
            }
        }
        Ok(digest)
    }

    // Hmm... I don't like this name...
    fn all_ok<T, E, F>(&mut self, predicate: F) -> Result<bool, E>
    where
        Self: Iterator<Item = Result<T, E>>,
        F: FnMut(T) -> bool,
    {
        self.process_results(|mut results| results.all(predicate))
    }

    // TODO(@TylerBloom): Write an example (or two) and write the docs.
    fn and_then_all<T, E, F>(&mut self, mut predicate: F) -> Result<bool, E>
    where
        Self: Iterator<Item = Result<T, E>>,
        F: FnMut(T) -> Result<bool, E>,
    {
        let mut digest = true;
        for val in self.by_ref() {
            digest &= val.and_then(&mut predicate)?;
            if !digest {
                break;
            }
        }
        Ok(digest)
    }

    /// ```rust
    /// use apollo_federation::utils::FallibleIterator;
    ///
    /// // A totally accurate prime checker
    /// fn is_prime(i: usize) -> Result<bool, ()> {
    ///   match i {
    ///     0 | 1 => Err(()), // 0 and 1 are neither prime or composite
    ///     2 | 3 => Ok(true),
    ///     _ => Ok(false), // Every other number is composite, I guess
    ///   }
    /// }
    ///
    /// assert_eq!(Ok(false), [].into_iter().fallible_any(is_prime));
    /// assert_eq!(Ok(true), (2..5).fallible_any(is_prime));
    /// assert_eq!(Ok(false), (4..5).fallible_any(is_prime));
    /// assert_eq!(Err(()), (1..4).fallible_any(is_prime));
    /// assert_eq!(Err(()), (1..5).fallible_any(is_prime));
    /// ```
    // TODO(@TylerBloom): Write an example (or two) and rewrite the docs.
    fn fallible_any<E, F>(&mut self, mut predicate: F) -> Result<bool, E>
    where
        F: FnMut(Self::Item) -> Result<bool, E>,
    {
        let mut digest = false;
        for val in self.by_ref() {
            digest |= predicate(val)?;
            if digest {
                break;
            }
        }
        Ok(digest)
    }

    // Hmm... I don't like this name...
    fn any_ok<T, E, F>(&mut self, predicate: F) -> Result<bool, E>
    where
        Self: Iterator<Item = Result<T, E>>,
        F: FnMut(T) -> bool,
    {
        self.process_results(|mut results| results.any(predicate))
    }

    // TODO(@TylerBloom): Write an example (or two) and write the docs.
    fn and_then_any<T, E, F>(&mut self, mut predicate: F) -> Result<bool, E>
    where
        Self: Iterator<Item = Result<T, E>>,
        F: FnMut(T) -> Result<bool, E>,
    {
        let mut digest = false;
        for val in self {
            digest |= val.and_then(&mut predicate)?;
            if digest {
                break;
            }
        }
        Ok(digest)
    }

    /// A convenience method that is equivalent to calling `.map(|result| result.and_then(fallible_fn))`.
    fn and_then<T, E, U, F>(self, map: F) -> AndThen<Self, F>
    where
        Self: Iterator<Item = Result<T, E>>,
        F: FnMut(T) -> Result<U, E>,
    {
        AndThen { iter: self, map }
    }

    /// A convenience method that is equivalent to calling `.map(|result| result.or_else(fallible_fn))`.
    fn or_else<T, E, EE, F>(self, map: F) -> OrElse<Self, F>
    where
        Self: Iterator<Item = Result<T, E>>,
        F: FnMut(E) -> Result<T, EE>,
    {
        OrElse { iter: self, map }
    }
}

impl<I: Iterator> FallibleIterator for I {}

/// The struct returned by [fallible_filter](FallibleIterator::fallible_filter).
pub struct FallibleFilter<I, F> {
    iter: I,
    predicate: F,
}

impl<I, F, E> Iterator for FallibleFilter<I, F>
where
    I: Iterator,
    F: FnMut(&I::Item) -> Result<bool, E>,
{
    type Item = Result<I::Item, E>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let val = self.iter.next()?;
            match (self.predicate)(&val) {
                Ok(true) => return Some(Ok(val)),
                Ok(false) => {}
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// The struct returned by [and_then_filter](FallibleIterator::and_then_filter).
pub struct AndThenFilter<I, F> {
    iter: I,
    predicate: F,
}

impl<I, F, T, E> Iterator for AndThenFilter<I, F>
where
    I: Iterator<Item = Result<T, E>>,
    F: FnMut(&T) -> Result<bool, E>,
{
    type Item = Result<T, E>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let val = self.iter.next()?;
            return match val {
                Err(e) => Some(Err(e)),
                Ok(val) => match (self.predicate)(&val) {
                    Ok(true) => Some(Ok(val)),
                    Ok(false) => continue,
                    Err(e) => Some(Err(e)),
                },
            };
        }
    }
}

pub struct AndThen<I, F> {
    iter: I,
    map: F,
}

impl<I, T, E, U, F> Iterator for AndThen<I, F>
where
    I: Iterator<Item = Result<T, E>>,
    F: FnMut(T) -> Result<U, E>,
{
    type Item = Result<U, E>;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|res| res.and_then(&mut self.map))
    }
}

pub struct OrElse<I, F> {
    iter: I,
    map: F,
}

impl<I, T, E, EE, F> Iterator for OrElse<I, F>
where
    I: Iterator<Item = Result<T, E>>,
    F: FnMut(E) -> Result<T, EE>,
{
    type Item = Result<T, EE>;

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|res| res.or_else(&mut self.map))
    }
}
