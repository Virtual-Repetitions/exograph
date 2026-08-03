// The point of this test is that imports are resolved through HTTP redirects, and that
// relative imports inside the fetched module resolve against the *post-redirect* URL.
//
// The unversioned deno.land/std URL below 302-redirects to a pinned version
// (https://deno.land/std@0.224.0/encoding/hex.ts), whose own "./_util.ts" import then has to
// resolve relative to that redirected location. Do not pin this to a version — that removes
// the redirect and leaves the test asserting nothing.
//
// This previously imported chiefbiiko/sha512 via denopkg.com. That host is now dead (it 404s,
// including its root), and the package's own deps.ts imports std-encoding from denopkg.com
// as well, so no URL pointing at that package can work. sha512 is computed with Deno's
// built-in Web Crypto instead, which keeps the redirect coverage without a third-party host.
import { encodeHex } from "https://deno.land/std/encoding/hex.ts";

export async function computeSha512(value: string): Promise<string> {
	const digest = await crypto.subtle.digest("SHA-512", new TextEncoder().encode(value));
	return encodeHex(new Uint8Array(digest));
}
