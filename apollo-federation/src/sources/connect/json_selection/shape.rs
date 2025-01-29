use apollo_compiler::collections::IndexMap;
use shape::Shape;

use super::ApplyToInternal;
use super::JSONSelection;

impl JSONSelection {
    /// Returns a [`ShapedSelection`] wrapping the JSONSelection, the currently
    /// known input shapes, and the computed output shape. This is typically
    /// only a starting point, meaning you will probably need to refine this
    /// [`ShapedSelection`] with [`ShapedSelection::refine`] or
    /// [`ShapedSelection::replace`] later.
    pub(crate) fn shaped_selection(&self) -> ShapedSelection {
        ShapedSelection::new(self.clone())
    }

    /// A quick way to get the most generic possible [`Shape`] for this
    /// [`JSONSelection`], without any additional named shapes specified.
    #[allow(dead_code)]
    pub(crate) fn shape(&self) -> Shape {
        self.output_shape(&IndexMap::default())
    }

    /// Called internally by [`ShapedSelection::compute`] to do the actual shape
    /// processing work. The root JSON input shape can be specified by defining
    /// the `$root` key in the `named_shapes` map.
    pub(crate) fn output_shape(&self, named_shapes: &IndexMap<String, Shape>) -> Shape {
        let input_shape = if let Some(root_shape) = named_shapes.get("$root") {
            root_shape.clone()
        } else {
            Shape::name("$root")
        };

        // At this level, $ and @ have the same value and shape.
        let dollar_shape = input_shape.clone();

        match self {
            Self::Named(selection) => {
                selection.compute_output_shape(input_shape, dollar_shape, named_shapes)
            }
            Self::Path(path_selection) => {
                path_selection.compute_output_shape(input_shape, dollar_shape, named_shapes)
            }
        }
    }
}

type Ref<T> = std::sync::Arc<T>;

#[derive(Debug, Clone)]
pub(crate) struct ShapedSelection {
    selection: Ref<JSONSelection>,
    named_shapes: IndexMap<String, Shape>,
    output_shape: Shape,
}

impl ShapedSelection {
    /// Takes ownership of the given [`JSONSelection`] and computes the output
    /// shape without any named shapes.
    pub(crate) fn new(selection: JSONSelection) -> Self {
        Self::compute(Ref::new(selection), IndexMap::default())
    }

    #[allow(dead_code)]
    pub(crate) fn selection(&self) -> &JSONSelection {
        self.selection.as_ref()
    }

    pub(crate) fn output_shape(&self) -> &Shape {
        &self.output_shape
    }

    /// Add new named shapes to this [`ShapedSelection`], using [`Shape::all`]
    /// to combine shapes wherever there are collisions with existing shapes.
    #[allow(dead_code)]
    pub(crate) fn refine(&self, new_named_shapes: IndexMap<String, Shape>) -> Self {
        let mut named_shapes = self.named_shapes.clone();
        for (new_name, new_shape) in new_named_shapes {
            if let Some(old_shape) = named_shapes.get_mut(&new_name) {
                *old_shape = Shape::all([old_shape.clone(), new_shape]);
            } else {
                named_shapes.insert(new_name, new_shape);
            }
        }
        Self::compute(self.selection.clone(), named_shapes)
    }

    /// Add new named shapes to this [`ShapedSelection`], replacing any existing
    /// shapes with the same name.
    #[allow(dead_code)]
    pub(crate) fn replace(&self, new_named_shapes: IndexMap<String, Shape>) -> Self {
        let mut named_shapes = self.named_shapes.clone();
        named_shapes.extend(new_named_shapes);
        Self::compute(self.selection.clone(), named_shapes)
    }

    fn compute(selection: Ref<JSONSelection>, named_shapes: IndexMap<String, Shape>) -> Self {
        let output_shape = selection.output_shape(&named_shapes);
        Self {
            selection,
            named_shapes,
            output_shape,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn pretty_print(&self) -> String {
        self.output_shape.pretty_print()
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::collections::IndexMap;
    use shape::Shape;

    use crate::selection;

    #[test]
    fn test_shaped_selection() {
        let selection = selection!("id name");
        let shaped_selection = selection.shaped_selection();
        assert_eq!(
            shaped_selection.pretty_print(),
            "{ id: $root.*.id, name: $root.*.name }"
        );
        assert_eq!(shaped_selection.selection(), &selection);
        assert_eq!(
            shaped_selection.output_shape().pretty_print(),
            "{ id: $root.*.id, name: $root.*.name }"
        );
        assert_eq!(&selection.shape(), shaped_selection.output_shape());
        {
            let refined_shaped_selection = shaped_selection.refine({
                let mut shapes = IndexMap::default();
                shapes.insert("$root".to_string(), Shape::empty_object());
                shapes
            });
            assert_eq!(
                refined_shaped_selection.pretty_print(),
                "{ id: None, name: None }"
            );
        }
        {
            let replaced_shaped_selection = shaped_selection.replace({
                let mut shapes = IndexMap::default();
                shapes.insert(
                    "$root".to_string(),
                    Shape::record({
                        let mut fields = Shape::empty_map();
                        fields.insert("id".to_string(), Shape::name("ID"));
                        fields.insert("name".to_string(), Shape::string());
                        fields
                    }),
                );
                shapes
            });
            assert_eq!(
                replaced_shaped_selection.pretty_print(),
                "{ id: ID, name: String }"
            );
        }
    }
}
