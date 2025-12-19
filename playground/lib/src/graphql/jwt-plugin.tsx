// JWT Debugging Plugin for GraphiQL
// Adds a tab to decode and inspect JWT tokens

import { useState, useEffect } from 'react';
import { decodeJwt, decodeProtectedHeader } from 'jose';

export function jwtDebugPlugin() {
  return {
    title: 'JWT',
    icon: () => (
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        width="16"
        height="16"
      >
        <rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
        <path d="M7 11V7a5 5 0 0 1 10 0v4" />
      </svg>
    ),
    content: () => <JWTDebugger />,
  };
}

interface DecodedJWT {
  header: Record<string, any>;
  payload: Record<string, any>;
  signature: string;
  raw: string;
}

function JWTDebugger() {
  const [jwtToken, setJwtToken] = useState('');
  const [decoded, setDecoded] = useState<DecodedJWT | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [autoDetect, setAutoDetect] = useState(true);
  const [autoSetHeader, setAutoSetHeader] = useState(true);

  // Auto-detect JWT from Authorization header in GraphiQL
  useEffect(() => {
    if (!autoDetect) return;

    const checkHeaders = () => {
      try {
        // Try to read from GraphiQL's headers editor storage
        const storageKey = Object.keys(localStorage).find(key => 
          key.includes('graphiql:headers')
        );
        
        if (storageKey) {
          const headers = localStorage.getItem(storageKey);
          if (headers) {
            const parsed = JSON.parse(headers);
            const authHeader = parsed.Authorization || parsed.authorization;
            
            if (authHeader && authHeader.startsWith('Bearer ')) {
              const token = authHeader.replace('Bearer ', '').trim();
              if (token && token !== jwtToken) {
                setJwtToken(token);
              }
            }
          }
        }
      } catch (e) {
        // Ignore errors in auto-detection
      }
    };

    // Check immediately and set up interval
    checkHeaders();
    const interval = setInterval(checkHeaders, 1000);
    
    return () => clearInterval(interval);
  }, [autoDetect, jwtToken]);

  // Auto-set Authorization header when JWT is manually entered
  useEffect(() => {
    if (!autoSetHeader || !jwtToken || jwtToken.trim() === '' || autoDetect) {
      return;
    }

    try {
      // Find and update the headers in localStorage
      const storageKey = Object.keys(localStorage).find(key => 
        key.includes('graphiql:headers')
      );
      
      if (storageKey) {
        const headers = localStorage.getItem(storageKey);
        const parsed = headers ? JSON.parse(headers) : {};
        parsed.Authorization = `Bearer ${jwtToken.trim()}`;
        localStorage.setItem(storageKey, JSON.stringify(parsed));
        
        // Trigger a storage event to notify GraphiQL
        window.dispatchEvent(new Event('storage'));
      }
    } catch (e) {
      // Ignore errors
    }
  }, [jwtToken, autoSetHeader, autoDetect]);

  // Decode JWT whenever token changes
  useEffect(() => {
    if (!jwtToken || jwtToken.trim() === '') {
      setDecoded(null);
      setError(null);
      return;
    }

    try {
      const parts = jwtToken.trim().split('.');
      
      if (parts.length !== 3) {
        throw new Error('Invalid JWT format. Expected 3 parts separated by dots.');
      }

      const header = decodeProtectedHeader(jwtToken);
      const payload = decodeJwt(jwtToken);
      
      setDecoded({
        header,
        payload,
        signature: parts[2],
        raw: jwtToken,
      });
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to decode JWT');
      setDecoded(null);
    }
  }, [jwtToken]);

  const formatTimestamp = (timestamp: number | undefined) => {
    if (!timestamp) return 'N/A';
    const date = new Date(timestamp * 1000);
    return `${date.toLocaleString()} (${timestamp})`;
  };

  const isExpired = (exp: number | undefined) => {
    if (!exp) return false;
    return Date.now() / 1000 > exp;
  };

  return (
    <div style={{ padding: '12px', height: '100%', overflow: 'auto' }}>
      <div style={{ marginBottom: '16px' }}>
        <div style={{ 
          display: 'flex', 
          justifyContent: 'space-between', 
          alignItems: 'center',
          marginBottom: '8px'
        }}>
          <label style={{ fontWeight: 'bold', fontSize: '14px' }}>
            JWT Token
          </label>
          <div style={{ display: 'flex', gap: '12px', fontSize: '12px' }}>
            <label style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
              <input
                type="checkbox"
                checked={autoDetect}
                onChange={(e) => setAutoDetect(e.target.checked)}
              />
              Auto-detect from Headers
            </label>
            <label style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
              <input
                type="checkbox"
                checked={autoSetHeader}
                onChange={(e) => setAutoSetHeader(e.target.checked)}
                disabled={autoDetect}
              />
              Auto-set Authorization header
            </label>
          </div>
        </div>
        <textarea
          value={jwtToken}
          onChange={(e) => setJwtToken(e.target.value)}
          placeholder="Paste your JWT token here or add it to Authorization header..."
          style={{
            width: '100%',
            minHeight: '80px',
            fontFamily: 'monospace',
            fontSize: '12px',
            padding: '8px',
            border: '1px solid hsl(var(--color-base))',
            borderRadius: '4px',
            backgroundColor: 'hsl(var(--color-base))',
            color: 'hsl(var(--color-neutral))',
            resize: 'vertical',
          }}
        />
      </div>

      {error && (
        <div style={{
          padding: '12px',
          backgroundColor: 'rgba(255, 0, 0, 0.1)',
          border: '1px solid rgba(255, 0, 0, 0.3)',
          borderRadius: '4px',
          marginBottom: '16px',
          color: 'hsl(var(--color-error))',
          fontSize: '13px',
        }}>
          <strong>Error:</strong> {error}
        </div>
      )}

      {decoded && (
        <div>
          {/* Header Section */}
          <div style={{ marginBottom: '20px' }}>
            <h3 style={{ 
              fontSize: '14px', 
              fontWeight: 'bold', 
              marginBottom: '8px',
              color: 'hsl(var(--color-primary))',
            }}>
              Header
            </h3>
            <pre style={{
              backgroundColor: 'hsl(var(--color-base))',
              padding: '12px',
              borderRadius: '4px',
              overflow: 'auto',
              fontSize: '12px',
              fontFamily: 'monospace',
            }}>
              {JSON.stringify(decoded.header, null, 2)}
            </pre>
          </div>

          {/* Payload Section */}
          <div style={{ marginBottom: '20px' }}>
            <h3 style={{ 
              fontSize: '14px', 
              fontWeight: 'bold', 
              marginBottom: '8px',
              color: 'hsl(var(--color-primary))',
            }}>
              Payload
            </h3>

            {/* Standard Claims */}
            {(decoded.payload.iss || decoded.payload.sub || decoded.payload.aud || 
              decoded.payload.exp || decoded.payload.iat) && (
              <div style={{ marginBottom: '12px' }}>
                <h4 style={{ fontSize: '13px', marginBottom: '6px', fontWeight: 600 }}>
                  Standard Claims
                </h4>
                <table style={{ 
                  width: '100%', 
                  fontSize: '12px',
                  borderCollapse: 'collapse',
                }}>
                  <tbody>
                    {decoded.payload.iss && (
                      <tr>
                        <td style={{ padding: '4px 8px', fontWeight: 'bold', width: '120px' }}>
                          Issuer (iss)
                        </td>
                        <td style={{ padding: '4px 8px', fontFamily: 'monospace' }}>
                          {String(decoded.payload.iss)}
                        </td>
                      </tr>
                    )}
                    {decoded.payload.sub && (
                      <tr>
                        <td style={{ padding: '4px 8px', fontWeight: 'bold' }}>
                          Subject (sub)
                        </td>
                        <td style={{ padding: '4px 8px', fontFamily: 'monospace' }}>
                          {String(decoded.payload.sub)}
                        </td>
                      </tr>
                    )}
                    {decoded.payload.aud && (
                      <tr>
                        <td style={{ padding: '4px 8px', fontWeight: 'bold' }}>
                          Audience (aud)
                        </td>
                        <td style={{ padding: '4px 8px', fontFamily: 'monospace' }}>
                          {Array.isArray(decoded.payload.aud) 
                            ? decoded.payload.aud.join(', ') 
                            : String(decoded.payload.aud)}
                        </td>
                      </tr>
                    )}
                    {decoded.payload.exp && (
                      <tr>
                        <td style={{ padding: '4px 8px', fontWeight: 'bold' }}>
                          Expires (exp)
                        </td>
                        <td style={{ 
                          padding: '4px 8px', 
                          fontFamily: 'monospace',
                          color: isExpired(decoded.payload.exp as number) ? 'hsl(var(--color-error))' : 'inherit'
                        }}>
                          {formatTimestamp(decoded.payload.exp as number)}
                          {isExpired(decoded.payload.exp as number) && ' ⚠️ EXPIRED'}
                        </td>
                      </tr>
                    )}
                    {decoded.payload.iat && (
                      <tr>
                        <td style={{ padding: '4px 8px', fontWeight: 'bold' }}>
                          Issued At (iat)
                        </td>
                        <td style={{ padding: '4px 8px', fontFamily: 'monospace' }}>
                          {formatTimestamp(decoded.payload.iat as number)}
                        </td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
            )}

            {/* Full Payload */}
            <h4 style={{ fontSize: '13px', marginBottom: '6px', fontWeight: 600 }}>
              Full Payload
            </h4>
            <pre style={{
              backgroundColor: 'hsl(var(--color-base))',
              padding: '12px',
              borderRadius: '4px',
              overflow: 'auto',
              fontSize: '12px',
              fontFamily: 'monospace',
            }}>
              {JSON.stringify(decoded.payload, null, 2)}
            </pre>
          </div>

          {/* Signature Section */}
          <div>
            <h3 style={{ 
              fontSize: '14px', 
              fontWeight: 'bold', 
              marginBottom: '8px',
              color: 'hsl(var(--color-primary))',
            }}>
              Signature
            </h3>
            <div style={{
              backgroundColor: 'hsl(var(--color-base))',
              padding: '12px',
              borderRadius: '4px',
              fontFamily: 'monospace',
              fontSize: '12px',
              wordBreak: 'break-all',
            }}>
              {decoded.signature}
            </div>
            <p style={{ 
              fontSize: '11px', 
              marginTop: '8px',
              opacity: 0.7,
            }}>
              Note: Signature verification requires the secret key and is not performed in the browser.
            </p>
          </div>
        </div>
      )}

      {!decoded && !error && jwtToken && (
        <div style={{
          padding: '20px',
          textAlign: 'center',
          opacity: 0.5,
        }}>
          Decoding...
        </div>
      )}

      {!decoded && !error && !jwtToken && (
        <div style={{
          padding: '20px',
          textAlign: 'center',
          opacity: 0.5,
        }}>
          <p>Enter a JWT token above or add an Authorization header in the Headers tab.</p>
          <p style={{ marginTop: '12px', fontSize: '12px' }}>
            Example: <code>Authorization: Bearer &lt;your-token&gt;</code>
          </p>
        </div>
      )}
    </div>
  );
}
