// Passkeys for links made with `glass link --passkey`: the host asks this browser to create
// one when the link is opened, then to sign a fresh challenge every time it signs in.

const b64 = (bytes) => btoa(String.fromCharCode(...new Uint8Array(bytes))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const unb64 = (s) => Uint8Array.from(atob(s.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));

/** Answers a host `passkey` message; `{ mode: 'error' }` if the browser declines. */
export async function answerPasskey({ mode, challenge, user, credential, uv = false }) {
  // The host may require Face ID / Touch ID / a PIN (device posture), not just a tap.
  const userVerification = uv ? 'required' : 'preferred';
  try {
    if (mode === 'create') {
      const cred = await navigator.credentials.create({
        publicKey: {
          challenge: unb64(challenge),
          rp: { name: 'Glass' },
          user: { id: crypto.getRandomValues(new Uint8Array(16)), name: user, displayName: user },
          pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
          authenticatorSelection: { residentKey: 'preferred', userVerification },
          timeout: 120_000,
        },
      });
      return {
        mode,
        id: cred.id,
        client_data: b64(cred.response.clientDataJSON),
        public_key: b64(cred.response.getPublicKey()),
        algorithm: cred.response.getPublicKeyAlgorithm(),
      };
    }
    const cred = await navigator.credentials.get({
      publicKey: {
        challenge: unb64(challenge),
        allowCredentials: [{ type: 'public-key', id: unb64(credential) }],
        userVerification,
        timeout: 120_000,
      },
    });
    return {
      mode,
      id: cred.id,
      client_data: b64(cred.response.clientDataJSON),
      authenticator_data: b64(cred.response.authenticatorData),
      signature: b64(cred.response.signature),
    };
  } catch (e) {
    return { mode: 'error', error: `${e.name ?? 'Error'}: ${e.message ?? e}` };
  }
}
