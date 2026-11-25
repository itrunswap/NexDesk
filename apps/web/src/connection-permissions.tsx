import { useRef, useState } from "react";
import { Capability } from "./protocol";

export type ConnectionPermissions = {
  input: boolean;
  capabilities: number;
};

const storageKey = "nexdesk.connection-permissions.v1";
const choices = [
  [Capability.Audio, "系统声音"],
  [Capability.Clipboard, "文本剪贴板"],
  [Capability.FileRead, "读取共享文件"],
  [Capability.FileWrite, "写入共享目录"],
  [Capability.Display, "调整远端分辨率"],
  [Capability.Text, "发送输入法文本"],
] as const;
const ordinary = choices.reduce((mask, [bit]) => mask | bit, 0);
const profiles = {
  viewing: { label: "仅观看", input: false, capabilities: Capability.Audio },
  everyday: {
    label: "日常远控",
    input: true,
    capabilities: Capability.Audio | Capability.Clipboard | Capability.Text,
  },
  full: { label: "完整远控", input: true, capabilities: ordinary },
} as const;
type Profile = keyof typeof profiles | "custom";

function normalized(
  input: boolean,
  capabilities: number,
): ConnectionPermissions {
  return {
    input,
    capabilities:
      capabilities & (input ? ordinary : ordinary & ~Capability.Text),
  };
}

export function loadConnectionPermissions(): ConnectionPermissions {
  try {
    const value: unknown = JSON.parse(
      localStorage.getItem(storageKey) ?? "null",
    );
    if (
      value &&
      typeof value === "object" &&
      "input" in value &&
      "capabilities" in value &&
      typeof value.input === "boolean" &&
      typeof value.capabilities === "number" &&
      Number.isInteger(value.capabilities) &&
      value.capabilities >= 0 &&
      value.capabilities <= ordinary
    ) {
      return normalized(value.input, value.capabilities);
    }
  } catch {
    // Private or restricted storage can still use in-memory preferences.
  }
  return normalized(profiles.everyday.input, profiles.everyday.capabilities);
}

export function storeConnectionPermissions(settings: ConnectionPermissions) {
  try {
    localStorage.setItem(storageKey, JSON.stringify(settings));
  } catch {
    /* In-memory settings apply. */
  }
}

export function PermissionsDialog({
  value,
  onSave,
  onClose,
  onForget,
}: {
  value: ConnectionPermissions;
  onSave: (permissions: ConnectionPermissions) => void;
  onClose: () => void;
  onForget?: () => void;
}) {
  const [custom, setCustom] = useState(value);
  const [profile, setProfile] = useState<Profile>(() => {
    for (const key of Object.keys(profiles) as Array<keyof typeof profiles>) {
      const selected = profiles[key];
      if (
        selected.input === value.input &&
        selected.capabilities === value.capabilities
      )
        return key;
    }
    return "custom";
  });
  const dialog = useRef<HTMLFormElement>(null);
  return (
    <div className="scrim">
      <form
        ref={dialog}
        role="dialog"
        aria-modal="true"
        aria-labelledby="permission-heading"
        className="dialog connection-permissions"
        onSubmit={(event) => {
          event.preventDefault();
          onSave(custom);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
          if (event.key === "Tab") {
            const controls = dialog.current?.querySelectorAll<HTMLElement>(
              "button:enabled, input:enabled, select:enabled",
            );
            if (!controls?.length) return;
            const first = controls.item(0);
            const last = controls.item(controls.length - 1);
            if (event.shiftKey && document.activeElement === first) {
              event.preventDefault();
              last.focus();
            } else if (!event.shiftKey && document.activeElement === last) {
              event.preventDefault();
              first.focus();
            }
          }
        }}
      >
        <h2 id="permission-heading">连接偏好</h2>
        <label>
          默认请求权限
          <select
            name="access-profile"
            autoFocus
            value={profile}
            onChange={(event) => {
              const choice = event.target.value as Profile;
              setProfile(choice);
              if (choice !== "custom") {
                const selected = profiles[choice];
                setCustom(normalized(selected.input, selected.capabilities));
              }
            }}
          >
            {Object.entries(profiles).map(([name, choice]) => (
              <option key={name} value={name}>
                {choice.label}
              </option>
            ))}
            <option value="custom">自定义</option>
          </select>
        </label>
        <p>实际权限由被控电脑批准，文件访问限于它设置的共享目录。</p>
        {profile === "custom" && (
          <div className="permission-options">
            <label className="check">
              <input
                type="checkbox"
                checked={custom.input}
                onChange={(event) => {
                  setCustom(
                    normalized(event.target.checked, custom.capabilities),
                  );
                }}
              />
              键鼠控制
            </label>
            {choices.map(([bit, name]) => (
              <label className="check" key={bit}>
                <input
                  type="checkbox"
                  checked={Boolean(custom.capabilities & bit)}
                  disabled={bit === Capability.Text && !custom.input}
                  onChange={(event) =>
                    setCustom(
                      normalized(
                        custom.input,
                        event.target.checked
                          ? custom.capabilities | bit
                          : custom.capabilities & ~bit,
                      ),
                    )
                  }
                />
                {name}
              </label>
            ))}
          </div>
        )}
        <div className="bar">
          {onForget && (
            <button type="button" onClick={onForget}>
              清除当前设备信任
            </button>
          )}
          <button type="button" onClick={onClose}>
            取消
          </button>
          <button className="primary" type="submit">
            保存
          </button>
        </div>
      </form>
    </div>
  );
}
