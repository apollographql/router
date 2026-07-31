export function headerPlugin(language, headerValue = () => "active") {
  return {
    handle() {
      return {
        tag: "proceed",
        val: {
          headers: [
            {
              tag: "set",
              val: {
                name: `x-wasm-${language}`,
                values: [headerValue()],
              },
            },
          ],
          context: [
            {
              tag: "set",
              val: {
                name: `wasm.${language}`,
                value: JSON.stringify({ language }),
              },
            },
          ],
          body: undefined,
        },
      };
    },
  };
}
