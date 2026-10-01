import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import btw from "@narumitw/pi-btw/dist/index.ts";

import { btwRequestHeaders } from "./btw-headers.ts";

export default function bundledBtw(pi: ExtensionAPI): void {
	btw(pi, { requestHeaders: btwRequestHeaders });
}
