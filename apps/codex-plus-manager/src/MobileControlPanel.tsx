import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  Cable,
  Check,
  Copy,
  LoaderCircle,
  QrCode,
  ShieldCheck,
  Smartphone,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";

export type MobileControlPhase =
  | "starting"
  | "relayConnecting"
  | "waitingPhone"
  | "startingCodex"
  | "ready"
  | "reconnecting"
  | "stopped";

export type MobileControlStatus = {
  enabled: boolean;
  running: boolean;
  hasKey: boolean;
  relayUrl: string;
  roomId: string;
  roomIdMasked: string;
  mobileUrl: string | null;
  phase: MobileControlPhase;
  message: string;
  sessionId: string | null;
  relayConnected: boolean;
  codexConnected: boolean;
  settingsError: string | null;
};

type CommandResult<T> = { status: string; message: string } & T;

const emptyStatus: MobileControlStatus = {
  enabled: false,
  running: false,
  hasKey: false,
  relayUrl: "wss://relay.jingziai.club/relay",
  roomId: "",
  roomIdMasked: "",
  mobileUrl: null,
  phase: "stopped",
  message: "desktop bridge stopped",
  sessionId: null,
  relayConnected: false,
  codexConnected: false,
  settingsError: null,
};

const previewStatus: MobileControlStatus = {
  enabled: true,
  running: true,
  hasKey: true,
  relayUrl: "wss://relay.jingziai.club/relay",
  roomId: "0123456789abcdef0123456789abcdef",
  roomIdMasked: "012345...cdef",
  mobileUrl: "https://relay.jingziai.club/relay/mobile#mx=preview",
  phase: "waitingPhone",
  message: "waiting for phone",
  sessionId: null,
  relayConnected: true,
  codexConnected: false,
  settingsError: null,
};

function isTauri() {
  return "__TAURI_INTERNALS__" in window;
}

type Props = {
  onNotice: (notice: { kind: "ok" | "error" | "info"; text: string }) => void;
};

function phaseMeta(phase: MobileControlPhase) {
  switch (phase) {
    case "starting":
      return { label: "启动中", tone: "info" };
    case "relayConnecting":
      return { label: "连接中继", tone: "info" };
    case "waitingPhone":
      return { label: "等待手机", tone: "warn" };
    case "startingCodex":
      return { label: "启动 Codex", tone: "info" };
    case "ready":
      return { label: "手机已接入", tone: "ok" };
    case "reconnecting":
      return { label: "重连中", tone: "warn" };
    case "stopped":
    default:
      return { label: "未开启", tone: "idle" };
  }
}

export function MobileControlPanel({ onNotice }: Props) {
  const [status, setStatus] = useState<MobileControlStatus>(emptyStatus);
  const [qrSvg, setQrSvg] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const refresh = useCallback(async () => {
    if (!isTauri()) {
      setStatus(previewStatus);
      return;
    }
    try {
      const result = await invoke<CommandResult<{ mobileControl: MobileControlStatus }>>(
        "get_mobile_control_status",
      );
      setStatus(result.mobileControl);
    } catch (error) {
      onNotice({ kind: "error", text: String(error) });
    }
  }, [onNotice]);

  useEffect(() => {
    void refresh();
    if (!isTauri()) return undefined;
    const timer = window.setInterval(() => {
      void refresh();
    }, 2500);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const toggle = async (next: boolean) => {
    if (!isTauri()) {
      setStatus((current) => ({
        ...current,
        enabled: next,
        running: next,
        phase: next ? "waitingPhone" : "stopped",
      }));
      return;
    }
    setBusy("toggle");
    try {
      const result = await invoke<CommandResult<{ mobileControl: MobileControlStatus }>>(
        next ? "enable_mobile_control" : "disable_mobile_control",
      );
      setStatus(result.mobileControl);
      if (!next) setQrSvg(null);
      onNotice({
        kind: result.status === "ok" ? "ok" : "error",
        text: result.message,
      });
    } catch (error) {
      onNotice({ kind: "error", text: String(error) });
    } finally {
      setBusy(null);
    }
  };

  const showQr = async () => {
    if (!isTauri()) {
      onNotice({ kind: "info", text: "预览模式不会生成真实二维码。" });
      return;
    }
    setBusy("qr");
    try {
      const result = await invoke<CommandResult<{ svg: string | null; mobileUrl: string | null }>>(
        "generate_mobile_qr_code",
      );
      if (result.status === "ok" && result.svg) {
        setQrSvg(result.svg);
        if (result.mobileUrl) {
          setStatus((current) => ({ ...current, mobileUrl: result.mobileUrl }));
        }
      } else {
        onNotice({ kind: "error", text: result.message });
      }
    } catch (error) {
      onNotice({ kind: "error", text: String(error) });
    } finally {
      setBusy(null);
    }
  };

  const copyLink = async () => {
    if (!status.mobileUrl) return;
    try {
      await navigator.clipboard.writeText(status.mobileUrl);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch (error) {
      onNotice({ kind: "error", text: `复制失败：${String(error)}` });
    }
  };

  const meta = useMemo(() => phaseMeta(status.phase), [status.phase]);
  const switchChecked = status.enabled || status.running;
  const stateLabel = status.running ? meta.label : status.enabled ? "等待启动" : "未开启";

  return (
    <section className={`mobile-card ${status.running ? "enabled" : ""}`}>
      <div className="mobile-heading">
        <div className="mobile-title">
          <span className="mobile-icon">
            <Smartphone size={17} />
          </span>
          <div>
            <strong>手机远程控制</strong>
            <span>在手机上查看项目和会话，继续与本机 Codex 对话</span>
          </div>
        </div>
        <label className="switch-control">
          <input
            checked={switchChecked}
            disabled={busy !== null || (!status.hasKey && !switchChecked)}
            onChange={(event) => void toggle(event.target.checked)}
            type="checkbox"
          />
          <span aria-hidden="true" />
          <strong>{stateLabel}</strong>
        </label>
      </div>

      {status.settingsError ? (
        <p className="mobile-note warning">
          Manager 设置无法读取：{status.settingsError}。若手机桥接仍在运行，可先关闭手机控制；
          修复 settings.json 后再重新开启。
        </p>
      ) : !status.hasKey ? (
        <p className="mobile-note warning">先填写并应用 Mirror X Key，手机控制才能开启。</p>
      ) : null}

      {switchChecked ? (
        <>
          <div className="mobile-phase-row">
            <span className={`mobile-phase-badge ${meta.tone}`}>{meta.label}</span>
            <span className="mobile-phase-message">{status.message || "等待状态回报"}</span>
          </div>

          <div className="mobile-health-grid">
            <div className={`mobile-health-item ${status.relayConnected ? "ready" : ""}`}>
              <Cable size={15} />
              <div>
                <strong>中继链路</strong>
                <span>{status.relayConnected ? "已连通" : "未连通"}</span>
              </div>
            </div>
            <div className={`mobile-health-item ${status.codexConnected ? "ready" : ""}`}>
              <ShieldCheck size={15} />
              <div>
                <strong>Codex 会话桥</strong>
                <span>{status.codexConnected ? "已就绪" : "等待启动"}</span>
              </div>
            </div>
          </div>

          <div className="mobile-facts">
            <div>
              <span>配对房间</span>
              <code>{status.roomIdMasked || "—"}</code>
            </div>
            <div>
              <span>中继地址</span>
              <code>{status.relayUrl}</code>
            </div>
            <div>
              <span>当前手机会话</span>
              <code>{status.sessionId || "尚未接入"}</code>
            </div>
          </div>

          <div className="mobile-actions">
            <button
              className="secondary-button"
              disabled={busy !== null}
              onClick={() => void showQr()}
              type="button"
            >
              {busy === "qr" ? <LoaderCircle className="spin" size={16} /> : <QrCode size={16} />}
              显示手机二维码
            </button>
            <button
              className="secondary-button"
              disabled={!status.mobileUrl}
              onClick={() => void copyLink()}
              type="button"
            >
              {copied ? <Check size={16} /> : <Copy size={16} />}
              {copied ? "已复制" : "复制手机链接"}
            </button>
          </div>

          {qrSvg ? (
            <div className="mobile-qr">
              <div dangerouslySetInnerHTML={{ __html: qrSvg }} />
              <p>
                二维码只包含手机远程所需的派生凭证，不包含原始 API Key。
                电脑关闭手机控制或退出后，手机端会断开；重新打开后可再次扫码接入。
              </p>
            </div>
          ) : null}

          <p className="mobile-note">
            手机与电脑通过当前配对凭证连接。关闭手机控制或退出本工具后连接会断开，
            下次使用时重新开启并扫码即可。
          </p>
        </>
      ) : null}
    </section>
  );
}
