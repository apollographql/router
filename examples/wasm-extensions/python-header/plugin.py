from wit_world import exports
from wit_world.exports import hooks


class Hooks(exports.Hooks):
    def handle(self, event: hooks.Event):
        return hooks.Outcome_Proceed(
            hooks.Mutation(
                headers=[
                    hooks.HeaderOperation_Set(
                        hooks.Header(
                            name="x-wasm-python",
                            values=["active"],
                        )
                    )
                ],
                context=[
                    hooks.ContextOperation_Set(
                        hooks.ContextEntry(
                            name="wasm.python",
                            value='{"language":"python"}',
                        )
                    )
                ],
                method=None,
                uri=None,
                body=None,
            )
        )
