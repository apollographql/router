//! Mutable cursors over a document under construction.

use crate::builder::{NewValue, PathSegment, ValueBuilder};
use crate::error::JsonError;
use crate::node::{Child, NodeId};

/// A mutable cursor into a [`ValueBuilder`].
///
/// Navigating to a cursor localizes the copy-on-write spine once — a
/// path-copy if the document was shared, a no-op if the builder owns it — so
/// every edit at the cursor is a local operation with no re-descent from the
/// root. Cursors chain by value; the borrow of the builder moves with them,
/// so a cursor can never outlive the builder or observe edits made behind
/// its back.
///
/// # Example
/// ```
/// use apollo_json::Value;
///
/// let doc = Value::parse(br#"{"person":{"age":18,"tags":[]}}"#.to_vec()).unwrap();
/// let mut builder = doc.edit();
/// let mut person = builder.get_mut("person")?;
/// person.set("age", 19)?;
/// person.set("email", "bob@example.com")?;
/// let mut tags = person.get_mut("tags")?;
/// tags.push("verified")?;
/// assert_eq!(
///     builder.seal().to_string(),
///     r#"{"person":{"age":19,"tags":["verified"],"email":"bob@example.com"}}"#
/// );
/// # Ok::<(), apollo_json::JsonError>(())
/// ```
pub struct ValueMut<'b> {
    builder: &'b mut ValueBuilder,
    node: NodeId,
}

/// Creates a cursor; internal, so `ValueMut` exposes no constructor surface.
pub(crate) fn cursor(builder: &mut ValueBuilder, node: NodeId) -> ValueMut<'_> {
    ValueMut { builder, node }
}

impl std::fmt::Debug for ValueMut<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValueMut").finish_non_exhaustive()
    }
}

impl<'b> ValueMut<'b> {
    /// Writes `value` at `segment` of this value: replace-or-append for
    /// object keys, replace for in-range indexes, append for an index equal
    /// to the length.
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] when the segment does not apply here (key
    /// on a non-object, index out of range); [`JsonError::NonFiniteNumber`]
    /// for a non-finite float.
    pub fn set<'s, 'v>(
        &mut self,
        segment: impl Into<PathSegment<'s>>,
        value: impl Into<NewValue<'v>>,
    ) -> Result<(), JsonError> {
        self.builder
            .set_at(self.node, &segment.into(), value.into())
    }

    /// Removes `segment` of this value (an object member, or an array
    /// element shifting later elements left), returning whether anything was
    /// removed.
    pub fn remove<'s>(&mut self, segment: impl Into<PathSegment<'s>>) -> bool {
        self.builder.remove_at(self.node, &segment.into())
    }

    /// Appends `value` to this array.
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] when this value is not an array;
    /// [`JsonError::NonFiniteNumber`] for a non-finite float.
    pub fn push<'v>(&mut self, value: impl Into<NewValue<'v>>) -> Result<(), JsonError> {
        self.builder.push_at(self.node, value.into())
    }

    /// Moves the cursor one segment deeper, consuming it — chains keep the
    /// single mutable borrow of the builder. A missing object key is created
    /// as an empty object.
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] when the segment does not resolve (key on
    /// a non-object, index out of range).
    pub fn get_mut<'s>(
        self,
        segment: impl Into<PathSegment<'s>>,
    ) -> Result<ValueMut<'b>, JsonError> {
        let node = self.builder.navigate_mut(self.node, &segment.into())?;
        Ok(ValueMut {
            builder: self.builder,
            node,
        })
    }

    /// A cursor at `segment` of this value, borrowing rather than consuming
    /// this cursor. Unlike [`ValueMut::get_mut`], the borrow ends when the
    /// returned cursor is dropped, so sibling children can be visited one at
    /// a time — descend into element 0, drop that cursor, descend into
    /// element 1, and so on — without re-navigating from the root.
    ///
    /// # Errors
    /// [`JsonError::PathNotFound`] when the segment does not resolve (key on
    /// a non-object, index out of range).
    pub fn child_mut<'s>(
        &mut self,
        segment: impl Into<PathSegment<'s>>,
    ) -> Result<ValueMut<'_>, JsonError> {
        let node = self.builder.navigate_mut(self.node, &segment.into())?;
        Ok(cursor(self.builder, node))
    }

    /// A read-only view of this cursor's value, including writes not yet
    /// sealed; see [`BuilderRef`](crate::BuilderRef).
    pub fn value(&self) -> crate::BuilderRef<'_> {
        crate::peek::BuilderRef::new(self.builder, Child::local(self.node))
    }
}
