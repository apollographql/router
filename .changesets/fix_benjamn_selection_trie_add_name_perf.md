### Long connector selection paths no longer slow composition

Computing the output shape of a `@connect(selection:)` with a long path, such as `a: b.c.c.c...`, took time quartic in the number of path steps, because recording each step's input path rebuilt the whole path once per segment. Connector validation computes that shape, so a 128-step path took about 0.3s to compose and a 256-step path about 4s. Recording now walks each path once: 256 steps take about 6ms.

By [@benjamn](https://github.com/benjamn)
