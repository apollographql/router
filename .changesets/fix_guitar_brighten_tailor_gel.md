### Re-instate macOS Intel-based (x86_64) binary distribution ([Issue #4483](https://github.com/apollographql/router/issues/4483))

We've re-instated the macOS Intel-based binary production and distribution that we had stopped in v1.38.0 on account of our CI provider shutting down their own Intel machines in upcoming months.  Rather than using an Intel-based machine, we will rely on the Xcode-supported cross-compilation to produce _two_ separate binaries, both created and tested only with an ARM-based Mac.

We will likely have to re-visit this deprecation in the future as libaries and hardware continues to move on but, as of today, there are still just enough users who are still reliant on Intel-based laptops that it warrants continuing our investment in the architecture.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/4605