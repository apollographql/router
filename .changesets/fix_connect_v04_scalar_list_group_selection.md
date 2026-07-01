### Connectors: don't flag scalar-list selections as group selections (connect v0.4) ([PR #9636](https://github.com/apollographql/router/pull/9636))

Composition with connect v0.4 reported a spurious `GROUP_SELECTION_IS_NOT_OBJECT` error for a renamed
arrow-method projection over a nested-list scalar field — e.g. `data: data->map(@->map(@->toString))`
against `data: [[String]]` produced "selects a group `data {}`, but `ReportData.data` is of type `String`
which is not an object." The selection is a scalar projection, not a group selection, and the field is
`[[String]]`. The identical schema composed cleanly under connect v0.3.

Cause: the shape-based group-selection check treated every `Array`-shaped selection as a group selection,
then required the field's type to be an object. A list is now treated as a group selection only when its
element shape is itself a group (a list of objects), so a list of scalars validates cleanly. This is a
sibling fix to [PR #9619](https://github.com/apollographql/router/pull/9619), in the group-selection
detector rather than the seen-fields walker.

By [@benjamn](https://github.com/benjamn) in [PR #9636](https://github.com/apollographql/router/pull/9636)
