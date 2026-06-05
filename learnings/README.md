# Router Learnings

A rustlings-style interactive CLI for learning how Apollo Router works. Built for engineers, managers, and curious non-technical folks — no Rust experience required to *take* the course.

## Quick start

```sh
cargo run -p router-learnings
```

That's it. The tool walks you through 12 chapters interactively. Progress is saved between sessions.

## Commands

```sh
# Start (or resume) the interactive tour
cargo run -p router-learnings

# See all chapters and your progress
cargo run -p router-learnings -- list

# Jump to a specific chapter
cargo run -p router-learnings -- chapter 5

# Reset progress and start over
cargo run -p router-learnings -- reset
```

## What's covered

| # | Chapter | Audience |
|---|---------|----------|
| 1 | The Big Picture | Everyone |
| 2 | The Request Lifecycle | Everyone |
| 3 | Configuration | Everyone |
| 4 | The Plugin System | Engineers + curious others |
| 5 | Customization Options | Engineers + PMs |
| 6 | Observability | Everyone |
| 7 | Backpressure & Load Management | Everyone |
| 8 | Security | Everyone |
| 9 | GraphOS & Enterprise Features | Everyone |
| 10 | Releases & LTS Policy | Everyone |
| 11 | Testing the Router | Engineers |
| 12 | Apollo Connectors | Engineers + PMs |

Each chapter has 2–5 exercises mixing concept explanations, multiple-choice questions, and code exploration ("open this file and find X"). Engineer deep-dives are optional and marked clearly.

## Duration

~3–4 hours straight through. Most people split it over a few days.

## Adding or improving exercises

All content lives in `learnings/src/chapters.rs`. Each `fn ch##()` returns a `Chapter` with a list of `Exercise` structs. The data types are defined in `learnings/src/main.rs`. Adding a question is as simple as appending to the `questions: vec![...]` of an exercise.
