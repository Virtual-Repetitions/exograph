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
import { SecretAuthContext } from "../auth/secret/SecretAuthProvider";
import { createJwtForProfile } from "../auth/secret/jwt-utils";

const MODE_STORAGE_KEY = "exograph_playground_headers_mode_v1";
const PROFILE_STORAGE_KEY = "exograph_playground_headers_profile_v1";

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

interface HeaderProfileFormProps {
  headerName: string;
  cookieName?: string;
}

function HeaderProfileForm({ headerName, cookieName }: HeaderProfileFormProps) {
  const { config } = useContext(AuthConfigContext);
  const { signedIn } = useContext(SecretAuthContext);
  const [headers, setHeaders] = useHeadersEditorState();

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

  const applyProfileHeaders = useCallback(async () => {
    if (mode !== "profile" || !activeProfile) {
      return;
    }

    setStatus({ type: "pending" });

    const hasDocument = typeof document !== "undefined";

    if (activeProfile.mode === "generated" && !signedIn) {
      setStatus({
        type: "error",
        message: "Sign in to issue a JWT for generated profiles.",
      });
      const fallbackValue = Object.keys(activeProfile.headers ?? {}).length
        ? JSON.stringify(activeProfile.headers, null, 2)
        : "{}";
      setHeaders(fallbackValue);
      if (cookieName && hasDocument) {
        document.cookie = `${cookieName}=`;
      }
      return;
    }

    const { token, error } = await createJwtForProfile(activeProfile);

    if (error) {
      setStatus({ type: "error", message: error });
    } else {
      setStatus({ type: "idle" });
    }

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
    setHeaders(nextValue);
  }, [mode, activeProfile, headerName, cookieName, setHeaders, signedIn]);

  useEffect(() => {
    if (mode === "profile") {
      void applyProfileHeaders();
    }
  }, [mode, applyProfileHeaders, activeProfile]);

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
      setHeaders(customHeadersBackup.current || "{}");
      setStatus({ type: "idle" });
    },
    [headers, mode, setHeaders]
  );

  const handleProfileChange = useCallback((profileId: string) => {
    setSelectedProfileId(profileId);
  }, []);

  if (!profiles.length) {
    return null;
  }

  return (
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
    </div>
  );
}

export function HeaderProfileSelector({
  headerName = "Authorization",
  cookieName,
}: HeaderProfileSelectorProps) {
  const [container, setContainer] = useState<HTMLElement | null>(null);
  const createdRef = useRef(false);

  useEffect(() => {
    if (typeof document === "undefined") {
      return;
    }

    let disposed = false;
    let mountNode: HTMLElement | null = null;

    const ensureContainer = () => {
      if (disposed) {
        return;
      }

      const tool = document.querySelector<HTMLElement>(
        '.graphiql-editor-tool[aria-label="Headers"]'
      );

      if (!tool) {
        requestAnimationFrame(ensureContainer);
        return;
      }

      const existing = tool.querySelector<HTMLElement>(
        ":scope > .exo-header-profile-container"
      );

      if (existing) {
        mountNode = existing;
        createdRef.current = false;
      } else {
        mountNode = document.createElement("div");
        mountNode.className = "exo-header-profile-container";
        tool.insertBefore(mountNode, tool.firstChild ?? null);
        createdRef.current = true;
      }

      setContainer(mountNode);
    };

    ensureContainer();

    return () => {
      disposed = true;
      if (createdRef.current && mountNode?.parentElement) {
        mountNode.parentElement.removeChild(mountNode);
      }
    };
  }, []);

  if (!container) {
    return null;
  }

  return createPortal(
    <HeaderProfileForm headerName={headerName} cookieName={cookieName} />,
    container
  );
}
