import { headerValue } from "./target/javascript/plugin.js";

export const hooks = {
  handle() {
    return {
      tag: "proceed",
      val: {
        headers: [
          {
            tag: "set",
            val: { name: "x-wasm-java", values: [headerValue()] },
          },
        ],
        context: [
          {
            tag: "set",
            val: {
              name: "wasm.java",
              value: JSON.stringify({ language: "java" }),
            },
          },
        ],
        body: undefined,
      },
    };
  },
};
