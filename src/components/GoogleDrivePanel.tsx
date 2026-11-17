import { useCallback, useEffect, useRef, useState } from 'react';
import type { User } from 'firebase/auth';
import {
  AlertTriangle,
  CloudUpload,
  Download,
  FileAudio,
  FileCode2,
  FileJson,
  HardDrive,
  LogOut,
  Pencil,
  RefreshCw,
  Search,
  Trash2,
  X,
} from 'lucide-react';
import {
  clearCachedToken,
  getAccessToken,
  googleSignIn,
  initAuth,
  logout,
} from '../services/googleAuth';
import {
  deleteDriveFile,
  downloadDriveFileBlob,
  DriveAuthExpiredError,
  DriveFileItem,
  listDriveFiles,
  renameDriveFile,
  uploadDriveFile,
} from '../services/googleDrive';

export interface DriveThemeTokens {
  canvas: string;
  surface: string;
  border: string;
  textPrimary: string;
  textSecondary: string;
  textMuted: string;
  accent: string;
  accentFg: string;
  codeBg: string;
}

interface GoogleDrivePanelProps {
  theme: DriveThemeTokens;
  activeWorkbenchFile: {
    path: string;
    content: string;
  };
}

type PendingMutation =
  | { type: 'delete'; file: DriveFileItem }
  | { type: 'rename'; file: DriveFileItem; nextName: string };

const DEFAULT_CONFIG_TEMPLATE = {
  chime_minutes: [0, 30],
  chime_enabled: true,
  silent_enabled: true,
  silent_start: '00:00',
  silent_end: '08:00',
  meal_enabled: true,
  meal_times: ['11:30', '17:30'],
  meal_custom_audio_path: '',
  meal_play_chime_before_custom: true,
  volume: 80,
  theme: 'warm_lounge',
  auto_start: true,
};

function formatBytes(size?: string): string {
  if (!size) return '—';
  const n = Number(size);
  if (!Number.isFinite(n) || n <= 0) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}

function formatDate(iso?: string): string {
  if (!iso) return '—';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

export function GoogleDrivePanel({ theme, activeWorkbenchFile }: GoogleDrivePanelProps) {
  const [needsAuth, setNeedsAuth] = useState(true);
  const [user, setUser] = useState<User | null>(null);
  const [isLoggingIn, setIsLoggingIn] = useState(false);

  const [files, setFiles] = useState<DriveFileItem[]>([]);
  const [loadingFiles, setLoadingFiles] = useState(false);
  const [busyAction, setBusyAction] = useState(false);
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);

  const [queryText, setQueryText] = useState('');
  const [filterKind, setFilterKind] = useState<'all' | 'audio' | 'config'>('all');
  const [pendingMutation, setPendingMutation] = useState<PendingMutation | null>(null);

  const fileInputRef = useRef<HTMLInputElement | null>(null);

  const loadFiles = useCallback(
    async (explicitToken?: string) => {
      const token = explicitToken ?? (await getAccessToken());
      if (!token) {
        setNeedsAuth(true);
        return;
      }

      setLoadingFiles(true);
      setErrorMessage(null);
      try {
        const list = await listDriveFiles(token, { queryText, filterKind });
        setFiles(list);
      } catch (err) {
        if (err instanceof DriveAuthExpiredError) {
          clearCachedToken();
          setNeedsAuth(true);
          setErrorMessage(err.message);
        } else {
          setErrorMessage(err instanceof Error ? err.message : '读取 Google Drive 文件列表失败');
        }
      } finally {
        setLoadingFiles(false);
      }
    },
    [filterKind, queryText]
  );

  useEffect(() => {
    const unsubscribe = initAuth(
      (authedUser, token) => {
        setUser(authedUser);
        setNeedsAuth(false);
        void loadFiles(token);
      },
      () => {
        setUser(null);
        setNeedsAuth(true);
        setFiles([]);
      }
    );
    return () => unsubscribe();
  }, [loadFiles]);

  const handleLogin = async () => {
    setIsLoggingIn(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      const result = await googleSignIn();
      setUser(result.user);
      setNeedsAuth(false);
      await loadFiles(result.accessToken);
    } catch (err) {
      setErrorMessage(err instanceof Error ? err.message : 'Google 登录失败，请重试。');
    } finally {
      setIsLoggingIn(false);
    }
  };

  const handleLogout = async () => {
    await logout();
    setUser(null);
    setFiles([]);
    setNeedsAuth(true);
    setStatusMessage('已安全退出 Google 账号连接。');
  };

  const handleLocalFileUpload = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const selected = event.target.files?.[0];
    event.target.value = '';
    if (!selected) return;

    const token = await getAccessToken();
    if (!token) {
      setNeedsAuth(true);
      return;
    }

    setBusyAction(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      const uploaded = await uploadDriveFile(
        token,
        selected.name,
        selected.type || 'application/octet-stream',
        selected
      );
      setStatusMessage(`已成功上传「${uploaded.name}」到 Google Drive。`);
      await loadFiles(token);
    } catch (err) {
      if (err instanceof DriveAuthExpiredError) {
        clearCachedToken();
        setNeedsAuth(true);
      }
      setErrorMessage(err instanceof Error ? err.message : '上传文件失败');
    } finally {
      setBusyAction(false);
    }
  };

  const handleUploadWorkbenchFile = async () => {
    const token = await getAccessToken();
    if (!token) {
      setNeedsAuth(true);
      return;
    }
    const baseName = activeWorkbenchFile.path.split('/').pop() || 'source.txt';

    setBusyAction(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      const uploaded = await uploadDriveFile(
        token,
        `HalfHourChime_${baseName}`,
        'text/plain;charset=utf-8',
        activeWorkbenchFile.content
      );
      setStatusMessage(`已将当前源码文件备份至云端：${uploaded.name}`);
      await loadFiles(token);
    } catch (err) {
      if (err instanceof DriveAuthExpiredError) {
        clearCachedToken();
        setNeedsAuth(true);
      }
      setErrorMessage(err instanceof Error ? err.message : '备份源码文件失败');
    } finally {
      setBusyAction(false);
    }
  };

  const handleUploadConfigTemplate = async () => {
    const token = await getAccessToken();
    if (!token) {
      setNeedsAuth(true);
      return;
    }

    setBusyAction(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      const uploaded = await uploadDriveFile(
        token,
        'half_hour_chime_config.json',
        'application/json',
        JSON.stringify(DEFAULT_CONFIG_TEMPLATE, null, 2)
      );
      setStatusMessage(`已上传网吧默认配置模板：${uploaded.name}`);
      await loadFiles(token);
    } catch (err) {
      if (err instanceof DriveAuthExpiredError) {
        clearCachedToken();
        setNeedsAuth(true);
      }
      setErrorMessage(err instanceof Error ? err.message : '上传配置模板失败');
    } finally {
      setBusyAction(false);
    }
  };

  const handleDownload = async (file: DriveFileItem) => {
    const token = await getAccessToken();
    if (!token) {
      setNeedsAuth(true);
      return;
    }

    setBusyAction(true);
    setErrorMessage(null);
    try {
      const blob = await downloadDriveFileBlob(token, file);
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = file.mimeType.startsWith('application/vnd.google-apps.')
        ? `${file.name}.pdf`
        : file.name;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
      setStatusMessage(`已开始下载「${file.name}」。`);
    } catch (err) {
      if (err instanceof DriveAuthExpiredError) {
        clearCachedToken();
        setNeedsAuth(true);
      }
      setErrorMessage(err instanceof Error ? err.message : '下载文件失败');
    } finally {
      setBusyAction(false);
    }
  };

  const executePendingMutation = async () => {
    if (!pendingMutation) return;
    const token = await getAccessToken();
    if (!token) {
      setPendingMutation(null);
      setNeedsAuth(true);
      return;
    }

    setBusyAction(true);
    setErrorMessage(null);
    setStatusMessage(null);
    try {
      if (pendingMutation.type === 'delete') {
        await deleteDriveFile(token, pendingMutation.file.id);
        setStatusMessage(`已从 Google Drive 删除「${pendingMutation.file.name}」。`);
      } else {
        const trimmed = pendingMutation.nextName.trim();
        if (!trimmed) {
          setErrorMessage('文件名不能为空。');
          setBusyAction(false);
          return;
        }
        const updated = await renameDriveFile(token, pendingMutation.file.id, trimmed);
        setStatusMessage(`已将文件重命名为「${updated.name}」。`);
      }
      setPendingMutation(null);
      await loadFiles(token);
    } catch (err) {
      if (err instanceof DriveAuthExpiredError) {
        clearCachedToken();
        setNeedsAuth(true);
        setPendingMutation(null);
      }
      setErrorMessage(err instanceof Error ? err.message : '操作失败');
    } finally {
      setBusyAction(false);
    }
  };

  if (needsAuth) {
    return (
      <div className="mx-auto max-w-2xl py-8">
        <div
          className="rounded-xl border p-8 text-center"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div
            className="mx-auto mb-4 flex h-14 w-14 items-center justify-center rounded-2xl"
            style={{ backgroundColor: theme.codeBg, border: `1px solid ${theme.border}` }}
          >
            <HardDrive className="h-7 w-7" style={{ color: theme.accent }} />
          </div>
          <h2 className="text-xl font-bold tracking-tight">连接 Google Drive 云端空间</h2>
          <p className="mx-auto mt-2.5 max-w-md text-sm leading-relaxed" style={{ color: theme.textSecondary }}>
            登录您的 Google 账号后，可在经您授权的情况下浏览、下载与管理云端配置文件（<code className="font-mono">config.json</code>）、自定义餐饮播报音频及工程源码备份。
          </p>

          {errorMessage && (
            <div className="mx-auto mt-4 max-w-md rounded-lg border border-red-400/40 bg-red-500/10 px-4 py-2.5 text-xs text-red-500">
              {errorMessage}
            </div>
          )}

          <div className="mt-6 flex justify-center">
            <button
              type="button"
              onClick={handleLogin}
              disabled={isLoggingIn}
              className="gsi-material-button"
            >
              <div className="gsi-material-button-state" />
              <div className="gsi-material-button-content-wrapper">
                <div className="gsi-material-button-icon">
                  <svg
                    version="1.1"
                    xmlns="http://www.w3.org/2000/svg"
                    viewBox="0 0 48 48"
                    style={{ display: 'block' }}
                  >
                    <path
                      fill="#EA4335"
                      d="M24 9.5c3.54 0 6.71 1.22 9.21 3.6l6.85-6.85C35.9 2.38 30.47 0 24 0 14.62 0 6.51 5.38 2.56 13.22l7.98 6.19C12.43 13.72 17.74 9.5 24 9.5z"
                    />
                    <path
                      fill="#4285F4"
                      d="M46.98 24.55c0-1.57-.15-3.09-.38-4.55H24v9.02h12.94c-.58 2.96-2.26 5.48-4.78 7.18l7.73 6c4.51-4.18 7.09-10.36 7.09-17.65z"
                    />
                    <path
                      fill="#FBBC05"
                      d="M10.53 28.59c-.48-1.45-.76-2.99-.76-4.59s.27-3.14.76-4.59l-7.98-6.19C.92 16.46 0 20.12 0 24c0 3.88.92 7.54 2.56 10.78l7.97-6.19z"
                    />
                    <path
                      fill="#34A853"
                      d="M24 48c6.48 0 11.93-2.13 15.89-5.81l-7.73-6c-2.15 1.45-4.92 2.3-8.16 2.3-6.26 0-11.57-4.22-13.47-9.91l-7.98 6.19C6.51 42.62 14.62 48 24 48z"
                    />
                    <path fill="none" d="M0 0h48v48H0z" />
                  </svg>
                </div>
                <span className="gsi-material-button-contents">
                  {isLoggingIn ? '正在连接 Google...' : 'Sign in with Google'}
                </span>
                <span style={{ display: 'none' }}>Sign in with Google</span>
              </div>
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="space-y-5">
      {/* 顶部账号与快捷云备份栏 */}
      <div
        className="flex flex-wrap items-center justify-between gap-4 rounded-xl border p-4"
        style={{ borderColor: theme.border, backgroundColor: theme.surface }}
      >
        <div className="flex items-center gap-3">
          <div
            className="flex h-10 w-10 items-center justify-center rounded-xl"
            style={{ backgroundColor: theme.codeBg, border: `1px solid ${theme.border}` }}
          >
            <HardDrive className="h-5 w-5" style={{ color: theme.accent }} />
          </div>
          <div>
            <div className="text-sm font-bold">
              {user?.displayName || 'Google Drive 已连接'}
            </div>
            <div className="text-xs" style={{ color: theme.textMuted }}>
              {user?.email || '已授权当前会话访问云端硬盘'}
            </div>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <input
            ref={fileInputRef}
            type="file"
            className="hidden"
            onChange={handleLocalFileUpload}
          />
          <button
            type="button"
            disabled={busyAction}
            onClick={() => fileInputRef.current?.click()}
            className="flex cursor-pointer items-center gap-1.5 rounded-lg px-3.5 py-2 text-xs font-semibold transition-opacity hover:opacity-90 disabled:opacity-50"
            style={{ backgroundColor: theme.accent, color: theme.accentFg }}
          >
            <CloudUpload className="h-3.5 w-3.5" />
            <span>上传本地音频/配置</span>
          </button>

          <button
            type="button"
            disabled={busyAction}
            onClick={handleUploadConfigTemplate}
            className="flex cursor-pointer items-center gap-1.5 rounded-lg border px-3 py-2 text-xs font-medium transition-opacity hover:opacity-80 disabled:opacity-50"
            style={{ borderColor: theme.border, color: theme.textPrimary }}
          >
            <FileJson className="h-3.5 w-3.5" />
            <span>上传网吧配置模板</span>
          </button>

          <button
            type="button"
            disabled={busyAction}
            onClick={handleUploadWorkbenchFile}
            className="flex cursor-pointer items-center gap-1.5 rounded-lg border px-3 py-2 text-xs font-medium transition-opacity hover:opacity-80 disabled:opacity-50"
            style={{ borderColor: theme.border, color: theme.textPrimary }}
          >
            <FileCode2 className="h-3.5 w-3.5" />
            <span>备份当前源码 ({activeWorkbenchFile.path.split('/').pop()})</span>
          </button>

          <button
            type="button"
            onClick={handleLogout}
            className="flex cursor-pointer items-center gap-1.5 rounded-lg border px-3 py-2 text-xs font-medium transition-opacity hover:opacity-80"
            style={{ borderColor: theme.border, color: theme.textMuted }}
          >
            <LogOut className="h-3.5 w-3.5" />
            <span>断开连接</span>
          </button>
        </div>
      </div>

      {/* 提示信息 */}
      {errorMessage && (
        <div className="rounded-xl border border-red-400/40 bg-red-500/10 px-4 py-3 text-xs text-red-500">
          {errorMessage}
        </div>
      )}
      {statusMessage && (
        <div
          className="rounded-xl border px-4 py-3 text-xs font-medium"
          style={{
            borderColor: theme.accent,
            backgroundColor: theme.surface,
            color: theme.textPrimary,
          }}
        >
          {statusMessage}
        </div>
      )}

      {/* 搜索与筛选控制条 */}
      <div
        className="flex flex-wrap items-center justify-between gap-3 rounded-xl border p-4"
        style={{ borderColor: theme.border, backgroundColor: theme.surface }}
      >
        <div className="flex flex-1 items-center gap-2 min-w-[220px]">
          <div
            className="flex flex-1 items-center gap-2 rounded-lg border px-3 py-2"
            style={{ borderColor: theme.border, backgroundColor: theme.codeBg }}
          >
            <Search className="h-4 w-4 shrink-0" style={{ color: theme.textMuted }} />
            <input
              type="text"
              value={queryText}
              onChange={(e) => setQueryText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void loadFiles();
              }}
              placeholder="搜索 Google Drive 文件名（回车搜索）..."
              className="w-full bg-transparent text-xs outline-none"
              style={{ color: theme.textPrimary }}
            />
          </div>
          <button
            type="button"
            onClick={() => void loadFiles()}
            disabled={loadingFiles}
            className="flex cursor-pointer items-center gap-1.5 rounded-lg border px-3 py-2 text-xs font-medium"
            style={{ borderColor: theme.border, color: theme.textPrimary }}
          >
            <RefreshCw className={`h-3.5 w-3.5 ${loadingFiles ? 'animate-spin' : ''}`} />
            <span>刷新</span>
          </button>
        </div>

        <div className="flex items-center gap-1.5">
          {(
            [
              { id: 'all', label: '全部文件' },
              { id: 'audio', label: '餐饮音频' },
              { id: 'config', label: '配置与文档' },
            ] as const
          ).map((item) => {
            const active = filterKind === item.id;
            return (
              <button
                key={item.id}
                type="button"
                onClick={() => setFilterKind(item.id)}
                className="cursor-pointer rounded-lg px-3 py-1.5 text-xs font-semibold transition-colors"
                style={{
                  backgroundColor: active ? theme.accent : theme.codeBg,
                  color: active ? theme.accentFg : theme.textSecondary,
                }}
              >
                {item.label}
              </button>
            );
          })}
        </div>
      </div>

      {/* 云端文件列表 */}
      <div
        className="overflow-hidden rounded-xl border"
        style={{ borderColor: theme.border, backgroundColor: theme.surface }}
      >
        {loadingFiles ? (
          <div className="py-16 text-center text-xs" style={{ color: theme.textMuted }}>
            正在加载 Google Drive 文件列表...
          </div>
        ) : files.length === 0 ? (
          <div className="py-16 text-center text-xs" style={{ color: theme.textMuted }}>
            当前筛选条件下暂无云端文件，您可以点击上方「上传本地音频/配置」或「上传网吧配置模板」。
          </div>
        ) : (
          <div className="divide-y" style={{ borderColor: theme.border }}>
            {files.map((file) => {
              const isAudio =
                file.mimeType.startsWith('audio/') ||
                /\.(wav|mp3|ogg|flac)$/i.test(file.name);
              return (
                <div
                  key={file.id}
                  className="flex flex-wrap items-center justify-between gap-4 px-5 py-3.5"
                  style={{ borderColor: theme.border }}
                >
                  <div className="flex min-w-0 flex-1 items-center gap-3">
                    {isAudio ? (
                      <FileAudio className="h-4 w-4 shrink-0" style={{ color: theme.accent }} />
                    ) : (
                      <FileCode2 className="h-4 w-4 shrink-0" style={{ color: theme.textMuted }} />
                    )}
                    <div className="min-w-0">
                      <div className="truncate text-sm font-semibold">{file.name}</div>
                      <div className="mt-0.5 flex flex-wrap gap-3 text-xs" style={{ color: theme.textMuted }}>
                        <span className="font-mono">{file.mimeType}</span>
                        <span>大小: {formatBytes(file.size)}</span>
                        <span>修改时间: {formatDate(file.modifiedTime)}</span>
                      </div>
                    </div>
                  </div>

                  <div className="flex items-center gap-2 shrink-0">
                    <button
                      type="button"
                      disabled={busyAction}
                      onClick={() => void handleDownload(file)}
                      className="flex cursor-pointer items-center gap-1 rounded-lg border px-2.5 py-1.5 text-xs font-medium hover:opacity-80 disabled:opacity-50"
                      style={{ borderColor: theme.border, color: theme.textPrimary }}
                    >
                      <Download className="h-3.5 w-3.5" />
                      <span>下载</span>
                    </button>

                    <button
                      type="button"
                      disabled={busyAction}
                      onClick={() =>
                        setPendingMutation({
                          type: 'rename',
                          file,
                          nextName: file.name,
                        })
                      }
                      className="flex cursor-pointer items-center gap-1 rounded-lg border px-2.5 py-1.5 text-xs font-medium hover:opacity-80 disabled:opacity-50"
                      style={{ borderColor: theme.border, color: theme.textSecondary }}
                    >
                      <Pencil className="h-3.5 w-3.5" />
                      <span>重命名</span>
                    </button>

                    <button
                      type="button"
                      disabled={busyAction}
                      onClick={() =>
                        setPendingMutation({
                          type: 'delete',
                          file,
                        })
                      }
                      className="flex cursor-pointer items-center gap-1 rounded-lg border border-red-400/40 px-2.5 py-1.5 text-xs font-medium text-red-500 hover:bg-red-500/10 disabled:opacity-50"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                      <span>删除</span>
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* 破坏性 / 修改类操作显式二次确认弹窗 */}
      {pendingMutation && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4">
          <div
            className="w-full max-w-md rounded-xl border p-6 shadow-xl"
            style={{ borderColor: theme.border, backgroundColor: theme.surface, color: theme.textPrimary }}
          >
            <div className="flex items-start justify-between gap-3">
              <div className="flex items-center gap-2.5">
                <AlertTriangle className="h-5 w-5 shrink-0 text-amber-500" />
                <h3 className="text-base font-bold">
                  {pendingMutation.type === 'delete'
                    ? '确认从 Google Drive 删除文件？'
                    : '确认重命名 Google Drive 文件？'}
                </h3>
              </div>
              <button
                type="button"
                onClick={() => setPendingMutation(null)}
                className="cursor-pointer rounded p-1 hover:opacity-70"
                style={{ color: theme.textMuted }}
              >
                <X className="h-4 w-4" />
              </button>
            </div>

            <p className="mt-3 text-xs leading-relaxed" style={{ color: theme.textSecondary }}>
              {pendingMutation.type === 'delete'
                ? `您即将永久删除云端文件「${pendingMutation.file.name}」。该操作不可撤销，请确认是否继续。`
                : `您即将修改云端文件「${pendingMutation.file.name}」的文件名。请输入新名称并点击确认保存。`}
            </p>

            {pendingMutation.type === 'rename' && (
              <div className="mt-4">
                <label className="mb-1.5 block text-xs font-medium" style={{ color: theme.textMuted }}>
                  新文件名
                </label>
                <input
                  type="text"
                  value={pendingMutation.nextName}
                  onChange={(e) =>
                    setPendingMutation({
                      ...pendingMutation,
                      nextName: e.target.value,
                    })
                  }
                  className="w-full rounded-lg border px-3 py-2 text-xs outline-none"
                  style={{
                    borderColor: theme.border,
                    backgroundColor: theme.codeBg,
                    color: theme.textPrimary,
                  }}
                />
              </div>
            )}

            <div className="mt-6 flex justify-end gap-2.5">
              <button
                type="button"
                disabled={busyAction}
                onClick={() => setPendingMutation(null)}
                className="cursor-pointer rounded-lg border px-4 py-2 text-xs font-semibold"
                style={{ borderColor: theme.border, color: theme.textSecondary }}
              >
                取消
              </button>
              <button
                type="button"
                disabled={busyAction}
                onClick={() => void executePendingMutation()}
                className="cursor-pointer rounded-lg px-4 py-2 text-xs font-semibold disabled:opacity-50"
                style={{
                  backgroundColor: pendingMutation.type === 'delete' ? '#dc2626' : theme.accent,
                  color: pendingMutation.type === 'delete' ? '#ffffff' : theme.accentFg,
                }}
              >
                {busyAction
                  ? '处理中...'
                  : pendingMutation.type === 'delete'
                    ? '确认删除'
                    : '确认修改'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
