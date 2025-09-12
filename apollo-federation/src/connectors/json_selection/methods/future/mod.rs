// The future module contains methods that are not yet exposed for use in
// JSONSelection strings in connector schemas, but have proposed implementations
// and tests. After careful review, they may one day move to public.

mod r#typeof;
pub(crate) use r#typeof::TypeOfMethod;
mod match_if;
pub(crate) use match_if::MatchIfMethod;
mod has;
pub(crate) use has::HasMethod;
mod keys;
pub(crate) use keys::KeysMethod;
mod values;
pub(crate) use values::ValuesMethod;
