import { headerValue } from "./target/javascript/plugin.js";
import { headerPlugin } from "../tooling/header-plugin.js";

export const hooks = headerPlugin("java", headerValue);
