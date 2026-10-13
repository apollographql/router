//! The [`json!`] construction macro.
//!
//! The muncher rules are adapted from `serde_json::json!`, rewritten to
//! assemble a pending [`NewValue`](crate::NewValue) tree instead of an owned
//! `serde_json::Value` tree: keys stay borrowed and the finished tree is
//! written into a single arena in one pass.

/// Builds a [`Value`](crate::Value) from a JSON literal, with the syntax of
/// `serde_json::json!`: expressions interpolate into value positions, keys
/// are string expressions, and trailing commas are accepted.
///
/// The whole literal is written into one fresh arena in a single pass — no
/// intermediate document per nesting level. Interpolated expressions convert
/// through [`NewValue`](crate::NewValue), so strings, numbers, booleans,
/// `Option`s (`None` becomes `null`), and [`Value`](crate::Value) handles all
/// work; an interpolated handle is adopted by reference, splicing the
/// existing subtree in without copying it. Non-finite floats become `null`,
/// as `serde_json` writes them.
///
/// # Example
/// ```
/// use apollo_json::json;
///
/// let who = "world";
/// let value = json!({
///     "greeting": format!("hello {who}"),
///     "ok": true,
///     "nested": { "xs": [1, 2.5, null, who] },
/// });
/// assert_eq!(
///     value.to_string(),
///     r#"{"greeting":"hello world","ok":true,"nested":{"xs":[1,2.5,null,"world"]}}"#
/// );
/// ```
#[macro_export]
macro_rules! json {
    ($($json:tt)+) => {
        $crate::Value::from($crate::__json_new_value!($($json)+))
    };
}

/// Implementation detail of [`json!`]: parses one JSON literal into a
/// [`NewValue`](crate::NewValue). The `@array` and `@object` rules munch
/// container bodies token by token, accumulating finished elements so that
/// JSON container syntax wins over the Rust expressions it resembles.
#[doc(hidden)]
#[macro_export]
macro_rules! __json_new_value {
    //////////////////////////////////////////////////////////////////////////
    // Array munching: @array [finished elements] remaining tokens.
    //////////////////////////////////////////////////////////////////////////

    // Done with trailing comma.
    (@array [$($elems:expr,)*]) => {
        ::std::vec![$($elems,)*]
    };

    // Done without trailing comma.
    (@array [$($elems:expr),*]) => {
        ::std::vec![$($elems),*]
    };

    // Next element is `null`.
    (@array [$($elems:expr,)*] null $($rest:tt)*) => {
        $crate::__json_new_value!(@array [$($elems,)* $crate::__json_new_value!(null)] $($rest)*)
    };

    // Next element is `true`.
    (@array [$($elems:expr,)*] true $($rest:tt)*) => {
        $crate::__json_new_value!(@array [$($elems,)* $crate::__json_new_value!(true)] $($rest)*)
    };

    // Next element is `false`.
    (@array [$($elems:expr,)*] false $($rest:tt)*) => {
        $crate::__json_new_value!(@array [$($elems,)* $crate::__json_new_value!(false)] $($rest)*)
    };

    // Next element is an array literal.
    (@array [$($elems:expr,)*] [$($array:tt)*] $($rest:tt)*) => {
        $crate::__json_new_value!(@array [$($elems,)* $crate::__json_new_value!([$($array)*])] $($rest)*)
    };

    // Next element is an object literal.
    (@array [$($elems:expr,)*] {$($map:tt)*} $($rest:tt)*) => {
        $crate::__json_new_value!(@array [$($elems,)* $crate::__json_new_value!({$($map)*})] $($rest)*)
    };

    // Next element is an expression followed by a comma.
    (@array [$($elems:expr,)*] $next:expr, $($rest:tt)*) => {
        $crate::__json_new_value!(@array [$($elems,)* $crate::__json_new_value!($next),] $($rest)*)
    };

    // Last element is an expression with no trailing comma.
    (@array [$($elems:expr,)*] $last:expr) => {
        $crate::__json_new_value!(@array [$($elems,)* $crate::__json_new_value!($last)])
    };

    // Comma after the most recent element.
    (@array [$($elems:expr),*] , $($rest:tt)*) => {
        $crate::__json_new_value!(@array [$($elems,)*] $($rest)*)
    };

    // Unexpected token after the most recent element.
    (@array [$($elems:expr),*] $unexpected:tt $($rest:tt)*) => {
        $crate::__json_unexpected!($unexpected)
    };

    //////////////////////////////////////////////////////////////////////////
    // Object munching: @object $object [current key] (remaining tokens) and a
    // copy of the remaining tokens for error reporting.
    //////////////////////////////////////////////////////////////////////////

    // Done.
    (@object $object:ident () () ()) => {};

    // Record the current entry, with a trailing comma after it.
    (@object $object:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        $object.push((($($key)+).into(), $value));
        $crate::__json_new_value!(@object $object () ($($rest)*) ($($rest)*));
    };

    // Current entry followed by an unexpected token.
    (@object $object:ident [$($key:tt)+] ($value:expr) $unexpected:tt $($rest:tt)*) => {
        $crate::__json_unexpected!($unexpected);
    };

    // Record the last entry, with no trailing comma after it.
    (@object $object:ident [$($key:tt)+] ($value:expr)) => {
        $object.push((($($key)+).into(), $value));
    };

    // Next value is `null`.
    (@object $object:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => {
        $crate::__json_new_value!(@object $object [$($key)+] ($crate::__json_new_value!(null)) $($rest)*);
    };

    // Next value is `true`.
    (@object $object:ident ($($key:tt)+) (: true $($rest:tt)*) $copy:tt) => {
        $crate::__json_new_value!(@object $object [$($key)+] ($crate::__json_new_value!(true)) $($rest)*);
    };

    // Next value is `false`.
    (@object $object:ident ($($key:tt)+) (: false $($rest:tt)*) $copy:tt) => {
        $crate::__json_new_value!(@object $object [$($key)+] ($crate::__json_new_value!(false)) $($rest)*);
    };

    // Next value is an array literal.
    (@object $object:ident ($($key:tt)+) (: [$($array:tt)*] $($rest:tt)*) $copy:tt) => {
        $crate::__json_new_value!(@object $object [$($key)+] ($crate::__json_new_value!([$($array)*])) $($rest)*);
    };

    // Next value is an object literal.
    (@object $object:ident ($($key:tt)+) (: {$($map:tt)*} $($rest:tt)*) $copy:tt) => {
        $crate::__json_new_value!(@object $object [$($key)+] ($crate::__json_new_value!({$($map)*})) $($rest)*);
    };

    // Next value is an expression followed by a comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr , $($rest:tt)*) $copy:tt) => {
        $crate::__json_new_value!(@object $object [$($key)+] ($crate::__json_new_value!($value)) , $($rest)*);
    };

    // Last value is an expression with no trailing comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr) $copy:tt) => {
        $crate::__json_new_value!(@object $object [$($key)+] ($crate::__json_new_value!($value)));
    };

    // Missing value for the last entry: trigger a reasonable error message.
    (@object $object:ident ($($key:tt)+) (:) $copy:tt) => {
        // "unexpected end of macro invocation"
        $crate::__json_new_value!();
    };

    // Missing colon and value for the last entry.
    (@object $object:ident ($($key:tt)+) () $copy:tt) => {
        // "unexpected end of macro invocation"
        $crate::__json_new_value!();
    };

    // Misplaced colon: report it on the colon token.
    (@object $object:ident () (: $($rest:tt)*) ($colon:tt $($copy:tt)*)) => {
        // "unexpected token"
        $crate::__json_unexpected!($colon);
    };

    // Found a comma inside a key: report it on the comma token.
    (@object $object:ident ($($key:tt)*) (, $($rest:tt)*) ($comma:tt $($copy:tt)*)) => {
        // "unexpected token"
        $crate::__json_unexpected!($comma);
    };

    // Key is fully parenthesized: an interpolated key expression.
    (@object $object:ident () (($key:expr) : $($rest:tt)*) $copy:tt) => {
        $crate::__json_new_value!(@object $object ($key) (: $($rest)*) (: $($rest)*));
    };

    // Munch one token into the current key.
    (@object $object:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => {
        $crate::__json_new_value!(@object $object ($($key)* $tt) ($($rest)*) ($($rest)*));
    };

    //////////////////////////////////////////////////////////////////////////
    // The main implementation: one JSON value.
    //////////////////////////////////////////////////////////////////////////

    (null) => {
        $crate::NewValue::Null
    };

    (true) => {
        $crate::NewValue::Bool(true)
    };

    (false) => {
        $crate::NewValue::Bool(false)
    };

    ([]) => {
        $crate::NewValue::Array(::std::vec::Vec::new())
    };

    ([ $($tt:tt)+ ]) => {
        $crate::NewValue::Array($crate::__json_new_value!(@array [] $($tt)+))
    };

    ({}) => {
        $crate::NewValue::Object(::std::vec::Vec::new())
    };

    ({ $($tt:tt)+ }) => {
        $crate::NewValue::Object({
            let mut object = ::std::vec::Vec::new();
            $crate::__json_new_value!(@object object () ($($tt)+) ($($tt)+));
            object
        })
    };

    // Any other expression interpolates through the `NewValue` conversions.
    ($other:expr) => {
        $crate::NewValue::from($other)
    };
}

/// Implementation detail of [`json!`]: matches nothing, so invoking it with a
/// token reports "no rules expected this token" at that token's span.
#[doc(hidden)]
#[macro_export]
macro_rules! __json_unexpected {
    () => {};
}
