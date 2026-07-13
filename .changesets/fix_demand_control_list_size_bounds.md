### Treat list sizes as non-negative in demand control cost estimation

Demand control now bounds the list sizes it uses when estimating operation cost. Slicing-argument values (for example `first`) provided as negative integers are clamped to zero, and configured `list_size` defaults use a saturating conversion so that very large values cannot wrap. This keeps an operation's estimated cost from being computed lower than the work it represents.

By [@abernix](https://github.com/abernix)
