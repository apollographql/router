import { headerValue } from "./target/plugin.js";
import { headerPlugin } from "../tooling/header-plugin.js";

export const hooks = headerPlugin("scala", headerValue);
