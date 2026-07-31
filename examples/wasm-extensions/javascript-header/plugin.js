export const hooks = {
  handle(event) {
    return {
      tag: "proceed",
      val: {
        headers: [
          {
            tag: "set",
            val: { name: "x-wasm-javascript", values: ["active"] },
          },
        ],
        context: [
          {
            tag: "set",
            val: {
              name: "wasm.javascript",
              value: JSON.stringify({ language: "javascript" }),
            },
          },
        ],
        body: undefined,
      },
    };
  },
};
