### Fixed a query planner bug where some `__typename` selections could be missing in query plans

The query planner uses an optimization technique called "sibling typename", which attaches `__typename` selections to their sibling selections so the planner won't need to plan them separately. The bug was that, when there are multiple identical selections and one of them has a `__typename` attached, the query planner could pick the one without the attachment, effectively losing a `__typename` selection. The query planner now favors the one with a `__typename` attached, so that the attached `__typename` selections won't be lost anymore.

By [@duckki](https://github.com/duckki) in https://github.com/apollographql/router/pull/6824
