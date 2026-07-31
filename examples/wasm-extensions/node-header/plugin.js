export const hooks = {
  handle(event) {
    return {
      tag: "proceed",
      val: {
        headers: [
          {
            tag: "set",
            val: { name: "x-wasm-node", values: ["active"] },
          },
        ],
        context: [
          {
            tag: "set",
            val: {
              name: "wasm.node",
              value: JSON.stringify({ language: "node" }),
            },
          },
        ],
        body: undefined,
      },
    };
  },
};
