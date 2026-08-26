import {
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { useHeadersEditorState } from "@graphiql/react";

import { AuthConfigContext } from "../auth/secret/AuthConfigProvider";
import { createJwtForProfile } from "../auth/secret/jwt-utils";

const MODE_STORAGE_KEY = "exograph_playground_headers_mode_v1";
const PROFILE_STORAGE_KEY = "exograph_playground_headers_profile_v1";

// Generated profile tokens are minted with a 10-minute expiry (see jwt-utils);
// re-mint well before that so an idle tab keeps working.
const TOKEN_REFRESH_INTERVAL_MS = 4 * 60 * 1000;

type Mode = "custom" | "profile";

function readModeFromStorage(): Mode {
  if (typeof window === "undefined") {
    return "custom";
  }
  const stored = window.localStorage.getItem(MODE_STORAGE_KEY);
  return stored === "profile" ? "profile" : "custom";
}

function readProfileIdFromStorage(): string | undefined {
  if (typeof window === "undefined") {
    return undefined;
  }
  return window.localStorage.getItem(PROFILE_STORAGE_KEY) || undefined;
}

interface HeaderProfileSelectorProps {
  headerName?: string;
  cookieName?: string;
}

// The GraphiQL editor-tools section is a single element whose aria-label flips
// between "Variables" and "Headers" with the active tab, so the profile FORM
// can only be shown while the Headers tab is active. The header-application
// logic must NOT live behind that condition — it has to run on load and on
// profile changes even if the Headers tab is never opened. Hence this
// component always mounts (and runs) HeaderProfileForm, which itself portals
// its UI into the tool section only while the section shows Headers.
export function HeaderProfileSelector({
  headerName = "Authorization",
  cookieName,
}: HeaderProfileSelectorProps) {
  return <HeaderProfileForm headerName={headerName} cookieName={cookieName} />;
}

interface HeaderProfileFormProps {
  headerName: string;
  cookieName?: string;
}

function useHeadersToolContainer(): HTMLElement | null {
  const [container, setContainer] = useState<HTMLElement | null>(null);

  useEffect(() => {
    if (typeof document === "undefined") {
      return;
    }

    const mountNode = document.createElement("div");
    mountNode.className = "exo-header-profile-container";

    const sync = () => {
      const tool = document.querySelector<HTMLElement>(
        '.graphiql-editor-tool[aria-label="Headers"]'
      );
      if (tool) {
        if (mountNode.parentElement !== tool) {
          tool.insertBefore(mountNode, tool.firstChild ?? null);
        }
        setContainer(mountNode);
      } else {
        mountNode.parentElement?.removeChild(mountNode);
        setContainer(null);
      }
    };

    sync();

    // The tool section's aria-label flips between "Variables" and "Headers"
    // as the user switches tabs, and the section itself appears only after
    // GraphiQL finishes rendering — watch for both.
    const observer = new MutationObserver(sync);
    observer.observe(document.body, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: ["aria-label"],
    });

    return () => {
      observer.disconnect();
      mountNode.parentElement?.removeChild(mountNode);
    };
  }, []);

  return container;
}

function HeaderProfileForm({ headerName, cookieName }: HeaderProfileFormProps) {
  const { config } = useContext(AuthConfigContext);
  const [headers, setHeaders] = useHeadersEditorState();
  const container = useHeadersToolContainer();

  const [mode, setMode] = useState<Mode>(() => readModeFromStorage());
  const [selectedProfileId, setSelectedProfileId] = useState<string | undefined>(
    () => readProfileIdFromStorage() ?? config.activeProfileId
  );
  const [status, setStatus] = useState<{ type: "idle" | "pending" | "error"; message?: string }>({
    type: "idle",
  });

  const customHeadersBackup = useRef<string>("{}");

  const profiles = config.profiles;

  const activeProfile = useMemo(() => {
    if (!profiles.length) {
      return undefined;
    }
    const explicit = selectedProfileId
      ? profiles.find((profile) => profile.id === selectedProfileId)
      : undefined;
    return explicit ?? profiles.find((profile) => profile.id === config.activeProfileId) ?? profiles[0];
  }, [profiles, selectedProfileId, config.activeProfileId]);

  // Ensure the selected profile stays valid if the config changes.
  useEffect(() => {
    if (!activeProfile) {
      setSelectedProfileId(config.activeProfileId);
    }
  }, [activeProfile, config.activeProfileId]);

  useEffect(() => {
    if (typeof window === "undefined") {
      return;
    }
    window.localStorage.setItem(MODE_STORAGE_KEY, mode);
  }, [mode]);

  useEffect(() => {
    if (typeof window === "undefined") {
      return;
    }
    if (selectedProfileId) {
      window.localStorage.setItem(PROFILE_STORAGE_KEY, selectedProfileId);
    } else {
      window.localStorage.removeItem(PROFILE_STORAGE_KEY);
    }
  }, [selectedProfileId]);

  // The headers editor's Monaco model may not exist yet right after mount;
  // report failure instead of throwing so the caller can retry (the setter's
  // identity changes once the model appears, re-triggering the apply effect).
  const trySetHeaders = useCallback(
    (value: string) => {
      try {
        setHeaders(value);
        return true;
      } catch {
        return false;
      }
    },
    [setHeaders]
  );

  const applyProfileHeaders = useCallback(async () => {
    if (mode !== "profile" || !activeProfile) {
      return;
    }

    setStatus({ type: "pending" });

    const hasDocument = typeof document !== "undefined";

    const { token, error } = await createJwtForProfile(activeProfile);

    const resolvedHeaders: Record<string, string> = { ...(activeProfile.headers ?? {}) };

    if (token) {
      if (cookieName && hasDocument) {
        document.cookie = `${cookieName}=${token}`;
      }

      if (headerName) {
        const withBearerPrefix = token.toLowerCase().startsWith("bearer ")
          ? token
          : `Bearer ${token}`;
        resolvedHeaders[headerName] = withBearerPrefix;
      }
    } else if (cookieName && hasDocument) {
      // Clear the cookie to avoid reusing a stale token.
      document.cookie = `${cookieName}=; Max-Age=0`;
    }

    const nextValue =
      Object.keys(resolvedHeaders).length > 0
        ? JSON.stringify(resolvedHeaders, null, 2)
        : "{}";
    const applied = trySetHeaders(nextValue);

    if (error) {
      setStatus({ type: "error", message: error });
    } else if (!applied) {
      setStatus({ type: "pending" });
    } else {
      setStatus({ type: "idle" });
    }
  }, [mode, activeProfile, headerName, cookieName, trySetHeaders]);

  useEffect(() => {
    if (mode === "profile") {
      void applyProfileHeaders();
    }
  }, [mode, applyProfileHeaders, activeProfile]);

  // Generated tokens expire; keep them fresh while the tab is open and
  // re-mint when the window regains focus (e.g. after the machine slept).
  useEffect(() => {
    if (mode !== "profile" || typeof window === "undefined") {
      return;
    }
    const refresh = () => void applyProfileHeaders();
    const intervalId = window.setInterval(refresh, TOKEN_REFRESH_INTERVAL_MS);
    window.addEventListener("focus", refresh);
    return () => {
      window.clearInterval(intervalId);
      window.removeEventListener("focus", refresh);
    };
  }, [mode, applyProfileHeaders]);

  const handleModeChange = useCallback(
    (nextMode: Mode) => {
      if (nextMode === mode) {
        return;
      }
      if (nextMode === "profile") {
        customHeadersBackup.current = headers || "{}";
        setMode("profile");
        return;
      }
      setMode("custom");
      trySetHeaders(customHeadersBackup.current || "{}");
      setStatus({ type: "idle" });
    },
    [headers, mode, trySetHeaders]
  );

  const handleProfileChange = useCallback((profileId: string) => {
    setSelectedProfileId(profileId);
  }, []);

  if (!profiles.length || !container) {
    return null;
  }

  return createPortal(
    <div className="exo-header-profile-selector">
      <div
        className="exo-header-profile-modes"
        role="radiogroup"
        aria-label="Headers mode"
      >
        <label className="exo-header-profile-option">
          <input
            type="radio"
            name="exo-header-mode"
            value="custom"
            checked={mode === "custom"}
            onChange={() => handleModeChange("custom")}
          />
          <span>Custom headers</span>
        </label>
        <label className="exo-header-profile-option">
          <input
            type="radio"
            name="exo-header-mode"
            value="profile"
            checked={mode === "profile"}
            onChange={() => handleModeChange("profile")}
          />
          <span>Use profile</span>
        </label>
      </div>
      <label className="exo-header-profile-option exo-header-profile-select-group">
        <span>Saved profile</span>
        <select
          className="exo-header-profile-select"
          value={activeProfile?.id ?? ""}
          onChange={(event) => handleProfileChange(event.target.value)}
          disabled={mode !== "profile"}
        >
          {profiles.map((profile) => (
            <option key={profile.id} value={profile.id}>
              {profile.name}
            </option>
          ))}
        </select>
      </label>
      {status.type === "pending" && (
        <span className="exo-header-profile-status" role="status">
          Applying…
        </span>
      )}
      {status.type === "error" && (
        <span className="exo-header-profile-status exo-header-profile-status--error">
          {status.message}
        </span>
      )}
    </div>,
    container
  );
}
