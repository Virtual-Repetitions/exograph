import * as jose from "jose";

import { AuthProfile } from "./SecretConfig";

export async function createJwtToken(
  claims: Record<string, unknown>,
  secret: string
): Promise<string | null> {
  if (secret === "") {
    return null;
  }

  const encodedSecret = new TextEncoder().encode(secret);
  const alg = "HS256";

  return await new jose.SignJWT(claims)
    .setProtectedHeader({ alg })
    .setIssuedAt()
    .setExpirationTime("10m")
    .sign(encodedSecret);
}

export async function createJwtForProfile(
  profile: AuthProfile
): Promise<{ token: string | null; error?: string }> {
  if (profile.mode === "static") {
    const token = profile.rawToken?.trim() ?? "";
    return { token: token || null };
  }

  const secret = profile.sharedSecret ?? "";
  if (!secret) {
    return { token: null, error: "Shared secret is not configured." };
  }

  try {
    const claims =
      profile.claims && profile.claims.trim()
        ? (JSON.parse(profile.claims) as Record<string, unknown>)
        : {};
    const token = await createJwtToken(claims, secret);
    return { token };
  } catch (error) {
    return {
      token: null,
      error: `Invalid claims JSON: ${(error as Error).message}`,
    };
  }
}
