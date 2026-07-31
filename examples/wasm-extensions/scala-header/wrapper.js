import { headerValue } from "./target/plugin.js";

export const hooks = {
  handle() {
    return {
      tag: "proceed",
      val: {
        headers: [
          {
            tag: "set",
            val: { name: "x-wasm-scala", values: [headerValue()] },
          },
        ],
        context: [
          {
            tag: "set",
            val: {
              name: "wasm.scala",
              value: JSON.stringify({ language: "scala" }),
            },
          },
        ],
        body: undefined,
      },
    };
  },
};
