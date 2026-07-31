// This URL 302-redirects to https://raw.githubusercontent.com/chiefbiiko/sha512/master/mod.ts,
// which is the point of this test: the module's own relative `./deps.ts` import must resolve
// against the post-redirect URL. Do not "simplify" this to the raw.githubusercontent URL.
import { sha512 } from 'https://github.com/chiefbiiko/sha512/raw/master/mod.ts';

import { encode as hexify } from "https://deno.land/std@0.192.0/encoding/hex.ts";

const decode = (d: Uint8Array) => new TextDecoder().decode(d);

export function computeSha512(value: string): string {
	const sha = sha512(value);
	if (typeof sha === 'string') {
		return sha;
	} else {
		return decode(hexify(sha));
	}
}

