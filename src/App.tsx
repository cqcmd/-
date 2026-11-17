/**
 * @license
 * SPDX-License-Identifier: Apache-2.0
 */

import { useState } from 'react';
import {
  Activity,
  Check,
  Copy,
  FileCode2,
  FolderOpen,
  Gauge,
  HardDrive,
  Layers,
  ShieldCheck,
} from 'lucide-react';
import { ErrorMonitorPanel } from './components/ErrorMonitorPanel';
import { GoogleDrivePanel } from './components/GoogleDrivePanel';
import { SystemOverheadPanel } from './components/SystemOverheadPanel';

import cargoToml from '../windows-native/Cargo.toml?raw';
import cargoConfig from '../windows-native/.cargo/config.toml?raw';
import appManifest from '../windows-native/app.manifest?raw';
import buildRs from '../windows-native/build.rs?raw';
import mainRs from '../windows-native/src/main.rs?raw';
import configRs from '../windows-native/src/config.rs?raw';
import schedulerRs from '../windows-native/src/scheduler.rs?raw';
import audioRs from '../windows-native/src/audio.rs?raw';
import ntpRs from '../windows-native/src/ntp.rs?raw';
import versionRs from '../windows-native/src/version.rs?raw';
import winSysRs from '../windows-native/src/win_sys.rs?raw';
import themeSlint from '../windows-native/ui/theme.slint?raw';
import appSlint from '../windows-native/ui/app.slint?raw';
import buildBat from '../windows-native/build-release.bat?raw';
import buildVersionTxt from '../windows-native/build_version.txt?raw';
import bumpVersionPy from '../windows-native/bump-version.py?raw';
import readmeMd from '../windows-native/README.md?raw';
import userManualMd from '../windows-native/USER_MANUAL.md?raw';
import acceptanceMd from '../windows-native/ACCEPTANCE_MATRIX.md?raw';

type ThemeId = 'obsidian' | 'win11' | 'warm';

interface ThemeTokens {
  id: ThemeId;
  label: string;
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

const THEMES: Record<ThemeId, ThemeTokens> = {
  obsidian: {
    id: 'obsidian',
    label: '暗夜曜石',
    canvas: '#0D1117',
    surface: '#151B24',
    border: '#303B4C',
    textPrimary: '#F8FAFC',
    textSecondary: '#E2E8F0',
    textMuted: '#A9B7CA',
    accent: '#38BDF8',
    accentFg: '#082F49',
    codeBg: '#0B0F14',
  },
  win11: {
    id: 'win11',
    label: '晨曦云母',
    canvas: '#F4F6F9',
    surface: '#FFFFFF',
    border: '#E2E8F0',
    textPrimary: '#0F172A',
    textSecondary: '#334155',
    textMuted: '#64748B',
    accent: '#2563EB',
    accentFg: '#FFFFFF',
    codeBg: '#F8FAFC',
  },
  warm: {
    id: 'warm',
    label: '暖金咖啡馆',
    canvas: '#F5F1E8',
    surface: '#FCFBF7',
    border: '#E2DAC6',
    textPrimary: '#292218',
    textSecondary: '#574B3A',
    textMuted: '#7C6E5A',
    accent: '#B45309',
    accentFg: '#FFFFFF',
    codeBg: '#F3EDE0',
  },
};

interface ProjectFile {
  path: string;
  group: 'Rust 核心逻辑' | 'Slint 多主题 UI' | '构建与清单' | '交付文档';
  summary: string;
  content: string;
}

const PROJECT_FILES: ProjectFile[] = [
  {
    path: 'windows-native/src/main.rs',
    group: 'Rust 核心逻辑',
    summary: '单实例、托盘循环、定制无边框标题栏拖拽/居中、250ms 调度、自动更新与超 50MB 内存强制回收',
    content: mainRs,
  },
  {
    path: 'windows-native/src/config.rs',
    group: 'Rust 核心逻辑',
    summary: '默认暖金咖啡馆主题、SAPI5/Edge TTS 引擎切换、节假日模板与 JSON 持久化校验',
    content: configRs,
  },
  {
    path: 'windows-native/src/scheduler.rs',
    group: 'Rust 核心逻辑',
    summary: '农历转换/节假日数据接口、跨日静默/静音判定、防重入与真实时间 TTS 文本生成',
    content: schedulerRs,
  },
  {
    path: 'windows-native/src/audio.rs',
    group: 'Rust 核心逻辑',
    summary: '提示音/节庆音/语音平滑淡入淡出、餐饮压低背景音乐及 SAPI5/Edge TTS 双引擎',
    content: audioRs,
  },
  {
    path: 'windows-native/src/ntp.rs',
    group: 'Rust 核心逻辑',
    summary: 'RFC 4330 SNTP UDP 客户端、SetSystemTime 同步与按需 UAC 提权流程',
    content: ntpRs,
  },
  {
    path: 'windows-native/src/version.rs',
    group: 'Rust 核心逻辑',
    summary: 'Z.W.&AI 出品信息与多更新源版本清单对比解析',
    content: versionRs,
  },
  {
    path: 'windows-native/src/win_sys.rs',
    group: 'Rust 核心逻辑',
    summary: '托盘扩展（检查更新/静音模式/打开日志目录）、DWM 圆角、SAPI 语音、自动更新与按天归档日志',
    content: winSysRs,
  },
  {
    path: 'windows-native/ui/theme.slint',
    group: 'Slint 多主题 UI',
    summary: '三套内置语义色彩令牌（默认暖金咖啡馆 / 暗夜曜石 / 晨曦云母）',
    content: themeSlint,
  },
  {
    path: 'windows-native/ui/app.slint',
    group: 'Slint 多主题 UI',
    summary: 'WindowStyle 圆角无边框标题栏、语音引擎切换、节假日专属报时与餐饮音频设置',
    content: appSlint,
  },
  {
    path: 'windows-native/Cargo.toml',
    group: '构建与清单',
    summary: 'Rust 依赖清单（含 MP3/FLAC/OGG 解码、Edge TTS WebSocket/SHA-256 与 Win32 API）',
    content: cargoToml,
  },
  {
    path: 'windows-native/.cargo/config.toml',
    group: '构建与清单',
    summary: 'MSVC 静态 CRT (+crt-static) 配置，保证零运行库依赖',
    content: cargoConfig,
  },
  {
    path: 'windows-native/app.manifest',
    group: '构建与清单',
    summary: 'Windows PerMonitorV2 高 DPI 感知与 asInvoker 权限声明',
    content: appManifest,
  },
  {
    path: 'windows-native/build.rs',
    group: '构建与清单',
    summary: '每次编译自动 +1 递增 build_version.txt（3段满10进位+日期）并编译 Slint 与资源',
    content: buildRs,
  },
  {
    path: 'windows-native/build_version.txt',
    group: '构建与清单',
    summary: '自动递增构建版本号状态文件（Z.W.&AI · 3段式满10进位 + YYYYMMDD）',
    content: buildVersionTxt,
  },
  {
    path: 'windows-native/build-release.bat',
    group: '构建与清单',
    summary: 'Windows 一键执行单元测试并产出 dist\\HalfHourChime.exe',
    content: buildBat,
  },
  {
    path: 'windows-native/bump-version.py',
    group: '构建与清单',
    summary: '手动同步递增 build_version.txt 与 Cargo.toml 版本号脚本',
    content: bumpVersionPy,
  },
  {
    path: 'windows-native/README.md',
    group: '交付文档',
    summary: '技术栈说明、编译环境准备与许可条款说明',
    content: readmeMd,
  },
  {
    path: 'windows-native/USER_MANUAL.md',
    group: '交付文档',
    summary: '面向门店/网吧非技术运维人员的快速上手与批量部署手册',
    content: userManualMd,
  },
  {
    path: 'windows-native/ACCEPTANCE_MATRIX.md',
    group: '交付文档',
    summary: '对应需求文档 AC-01 至 AC-12 的逐项实现与核验对照表',
    content: acceptanceMd,
  },
];

const GROUPS: ProjectFile['group'][] = [
  'Rust 核心逻辑',
  'Slint 多主题 UI',
  '构建与清单',
  '交付文档',
];

export default function App() {
  const [themeId, setThemeId] = useState<ThemeId>('warm');
  const [activeTab, setActiveTab] = useState<'workbench' | 'monitoring' | 'overhead' | 'drive'>(
    'workbench',
  );
  const [selectedPath, setSelectedPath] = useState<string>(PROJECT_FILES[0].path);
  const [copied, setCopied] = useState(false);

  const theme = THEMES[themeId];
  const currentVersion = `v${buildVersionTxt.trim()}`;
  const activeFile = PROJECT_FILES.find((f) => f.path === selectedPath) ?? PROJECT_FILES[0];

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(activeFile.content);
      setCopied(true);
      setTimeout(() => setCopied(false), 1800);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div
      className={`min-h-screen transition-colors duration-150${
        themeId === 'obsidian' ? ' dark-crisp-text' : ''
      }`}
      style={{ backgroundColor: theme.canvas, color: theme.textPrimary }}
    >
      {/* 顶部三段式精简栏 */}
      <header
        className="border-b px-6 py-4"
        style={{ borderColor: theme.border, backgroundColor: theme.surface }}
      >
        <div className="mx-auto flex max-w-7xl flex-wrap items-center justify-between gap-4">
          <div className="flex items-center gap-3 whitespace-nowrap shrink-0">
            <img
              src="/assets/app_icon_hd.png"
              alt="半整点提醒高清应用图标"
              referrerPolicy="no-referrer"
              className="h-8 w-8 rounded-lg object-contain"
            />
            <span className="text-lg font-bold tracking-tight">半整点提醒 · Windows 原生工程工作台</span>
          </div>

          <div className="flex items-center gap-3">
            {/* 视图模式切换：源码工作台 / 错误监控报告 / 系统开销分析 / Google Drive */}
            <div
              className="flex flex-wrap items-center gap-1 rounded-lg border p-1 shrink-0"
              style={{ borderColor: theme.border, backgroundColor: theme.canvas }}
            >
              <button
                type="button"
                onClick={() => setActiveTab('workbench')}
                className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-semibold transition-colors duration-150 shrink-0"
                style={{
                  backgroundColor: activeTab === 'workbench' ? theme.accent : 'transparent',
                  color: activeTab === 'workbench' ? theme.accentFg : theme.textSecondary,
                }}
              >
                <FileCode2 className="h-3.5 w-3.5" />
                <span>源码工作台</span>
              </button>
              <button
                type="button"
                onClick={() => setActiveTab('monitoring')}
                className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-semibold transition-colors duration-150 shrink-0"
                style={{
                  backgroundColor: activeTab === 'monitoring' ? theme.accent : 'transparent',
                  color: activeTab === 'monitoring' ? theme.accentFg : theme.textSecondary,
                }}
              >
                <Activity className="h-3.5 w-3.5" />
                <span>错误监控报告</span>
              </button>
              <button
                type="button"
                onClick={() => setActiveTab('overhead')}
                className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-semibold transition-colors duration-150 shrink-0"
                style={{
                  backgroundColor: activeTab === 'overhead' ? theme.accent : 'transparent',
                  color: activeTab === 'overhead' ? theme.accentFg : theme.textSecondary,
                }}
              >
                <Gauge className="h-3.5 w-3.5" />
                <span>系统开销分析</span>
              </button>
              <button
                type="button"
                onClick={() => setActiveTab('drive')}
                className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-semibold transition-colors duration-150 shrink-0"
                style={{
                  backgroundColor: activeTab === 'drive' ? theme.accent : 'transparent',
                  color: activeTab === 'drive' ? theme.accentFg : theme.textSecondary,
                }}
              >
                <HardDrive className="h-3.5 w-3.5" />
                <span>Google Drive</span>
              </button>
            </div>

            <nav
              className="hidden xl:flex items-center gap-3 text-xs"
              style={{ color: theme.textMuted }}
            >
              <span>Z.W.&amp;AI</span>
              <span aria-hidden="true">·</span>
              <span className="font-mono">{currentVersion}</span>
            </nav>

            {/* 实时三主题切换控件 */}
            <div
              className="flex items-center gap-1 rounded-lg border p-1 shrink-0"
              style={{ borderColor: theme.border, backgroundColor: theme.canvas }}
            >
              {(Object.keys(THEMES) as ThemeId[]).map((id) => {
                const isSelected = themeId === id;
                return (
                  <button
                    key={id}
                    type="button"
                    onClick={() => setThemeId(id)}
                    className="cursor-pointer whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-semibold transition-colors duration-150"
                    style={{
                      backgroundColor: isSelected ? theme.accent : 'transparent',
                      color: isSelected ? theme.accentFg : theme.textSecondary,
                    }}
                  >
                    {THEMES[id].label}
                  </button>
                );
              })}
            </div>
          </div>
        </div>
      </header>

      {/* 主内容区 */}
      {activeTab === 'drive' ? (
        <main className="mx-auto max-w-7xl px-6 py-6">
          <GoogleDrivePanel theme={theme} activeWorkbenchFile={activeFile} />
        </main>
      ) : activeTab === 'monitoring' ? (
        <main className="mx-auto max-w-7xl px-6 py-6">
          <ErrorMonitorPanel theme={theme} />
        </main>
      ) : activeTab === 'overhead' ? (
        <main className="mx-auto max-w-7xl px-6 py-6">
          <SystemOverheadPanel theme={theme} />
        </main>
      ) : (
      <main className="mx-auto grid max-w-7xl grid-cols-1 gap-6 px-6 py-6 lg:grid-cols-12">
        {/* 左侧边栏：工程模块索引 */}
        <aside className="space-y-4 lg:col-span-4">
          <div
            className="rounded-xl border p-4"
            style={{ borderColor: theme.border, backgroundColor: theme.surface }}
          >
            <div className="flex items-center gap-3.5">
              <img
                src="/assets/app_icon_hd.png"
                alt="半整点提醒高清桌面图标"
                referrerPolicy="no-referrer"
                className="h-14 w-14 shrink-0 rounded-xl object-contain"
              />
              <div className="min-w-0">
                <div className="flex items-center gap-2 text-sm font-semibold">
                  <Layers className="h-4 w-4 shrink-0" style={{ color: theme.accent }} />
                  <span>高清图标与版本规范</span>
                </div>
                <div className="mt-1 font-mono text-xs" style={{ color: theme.textSecondary }}>
                  Z.W.&amp;AI · {currentVersion}
                </div>
                <p className="mt-1 text-xs leading-relaxed" style={{ color: theme.textMuted }}>
                  已同步打包 16–256px 多分辨率 <code className="font-mono">app.ico</code>、512px 窗口图标与托盘图标。
                </p>
              </div>
            </div>
          </div>

          <div
            className="rounded-xl border p-3"
            style={{ borderColor: theme.border, backgroundColor: theme.surface }}
          >
            <div className="space-y-4">
              {GROUPS.map((group) => (
                <div key={group}>
                  <div
                    className="mb-1.5 flex items-center gap-1.5 px-2 text-xs font-semibold"
                    style={{ color: theme.textMuted }}
                  >
                    <FolderOpen className="h-3.5 w-3.5" />
                    <span>{group}</span>
                  </div>
                  <div className="space-y-1">
                    {PROJECT_FILES.filter((f) => f.group === group).map((file) => {
                      const active = file.path === activeFile.path;
                      return (
                        <button
                          key={file.path}
                          type="button"
                          onClick={() => setSelectedPath(file.path)}
                          className="flex w-full cursor-pointer items-center gap-2 rounded-lg px-2.5 py-2 text-left text-xs font-medium transition-colors duration-150"
                          style={{
                            backgroundColor: active ? theme.accent : 'transparent',
                            color: active ? theme.accentFg : theme.textSecondary,
                          }}
                        >
                          <FileCode2 className="h-3.5 w-3.5 shrink-0" />
                          <span className="truncate font-mono">{file.path.replace('windows-native/', '')}</span>
                        </button>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>
          </div>

          <div
            className="rounded-xl border p-4"
            style={{ borderColor: theme.border, backgroundColor: theme.surface }}
          >
            <div className="mb-2 flex items-center gap-2 text-sm font-semibold">
              <ShieldCheck className="h-4 w-4" style={{ color: theme.accent }} />
              <span>Windows 打包命令</span>
            </div>
            <pre
              className="overflow-x-auto rounded-lg border p-3 font-mono text-xs"
              style={{
                borderColor: theme.border,
                backgroundColor: theme.codeBg,
                color: theme.textSecondary,
              }}
            >
              {`cd windows-native\nbuild-release.bat`}
            </pre>
          </div>
        </aside>

        {/* 右侧主体：源码内容查阅器 */}
        <section className="lg:col-span-8">
          <div
            className="flex h-full flex-col overflow-hidden rounded-xl border"
            style={{ borderColor: theme.border, backgroundColor: theme.surface }}
          >
            <div
              className="flex flex-wrap items-center justify-between gap-4 border-b px-5 py-3.5"
              style={{ borderColor: theme.border }}
            >
              <div className="min-w-0">
                <div className="truncate font-mono text-sm font-bold">{activeFile.path}</div>
                <div className="mt-0.5 text-xs" style={{ color: theme.textMuted }}>
                  {activeFile.summary}
                </div>
              </div>
              <button
                type="button"
                onClick={handleCopy}
                className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-lg px-3.5 py-2 text-xs font-semibold transition-colors duration-150 shrink-0"
                style={{ backgroundColor: theme.accent, color: theme.accentFg }}
              >
                {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
                <span>{copied ? '已复制' : '复制文件内容'}</span>
              </button>
            </div>

            <div className="flex-1 overflow-auto p-4" style={{ backgroundColor: theme.codeBg }}>
              <pre
                className="font-mono text-xs leading-6"
                style={{ color: theme.textSecondary }}
              >
                <code>{activeFile.content}</code>
              </pre>
            </div>
          </div>
        </section>
      </main>
      )}
    </div>
  );
}
