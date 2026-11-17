import { useMemo, useRef, useState } from 'react';
import {
  AlertTriangle,
  Cpu,
  Download,
  Gauge,
  MemoryStick,
  RotateCcw,
  ShieldCheck,
  Upload,
  X,
} from 'lucide-react';
import {
  Area,
  CartesianGrid,
  ComposedChart,
  Legend,
  Line,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { MonitorThemeTokens } from './ErrorMonitorPanel';
import {
  buildProfileOverheadReport,
  HARDWARE_PROFILES,
  HardwareProfileId,
  parsePerfDebugLogs,
  PerfSamplePoint,
  PerfStateKind,
  STATE_META,
  SystemOverheadReport,
} from '../utils/perfMonitorParser';

interface SystemOverheadPanelProps {
  theme: MonitorThemeTokens;
}

interface OverheadTooltipItem {
  name?: string;
  value?: number | string;
  color?: string;
  dataKey?: string;
  payload?: PerfSamplePoint;
}

interface OverheadTooltipProps {
  active?: boolean;
  payload?: OverheadTooltipItem[];
  label?: string;
  theme: MonitorThemeTokens;
  mode: 'memory' | 'cpu';
}

function OverheadChartTooltip({ active, payload, label, theme, mode }: OverheadTooltipProps) {
  if (!active || !payload || payload.length === 0) return null;
  const point = payload[0]?.payload;

  return (
    <div
      className="max-w-xs rounded-lg border px-3.5 py-2.5 shadow-sm text-xs"
      style={{
        backgroundColor: theme.surface,
        borderColor: theme.border,
        color: theme.textPrimary,
      }}
    >
      <div className="mb-1.5 flex items-center justify-between gap-4 font-mono font-semibold">
        <span>{point?.timeLabel ?? label}</span>
        {point && (
          <span style={{ color: STATE_META[point.state].color }}>{point.stateLabel}</span>
        )}
      </div>
      <div className="space-y-1">
        {payload.map((item) => {
          const isCpu =
            item.dataKey === 'processCpuPct' || item.dataKey === 'systemCpuPct';
          const formatted =
            typeof item.value === 'number'
              ? isCpu
                ? `${item.value.toFixed(2)}%`
                : `${item.value.toFixed(2)} MB`
              : String(item.value ?? '0');

          return (
            <div
              key={String(item.dataKey ?? item.name)}
              className="flex items-center justify-between gap-5"
            >
              <span className="flex items-center gap-1.5" style={{ color: theme.textSecondary }}>
                <span
                  className="inline-block h-2 w-2 shrink-0 rounded-xs"
                  style={{ backgroundColor: item.color ?? theme.accent }}
                />
                <span>{item.name}</span>
              </span>
              <span className="font-mono tabular-nums font-semibold">{formatted}</span>
            </div>
          );
        })}
      </div>
      {point && (
        <div
          className="mt-2 border-t pt-1.5 text-[11px] leading-relaxed"
          style={{ borderColor: theme.border, color: theme.textMuted }}
        >
          {mode === 'memory' && point.reclaimedFromMb !== undefined ? (
            <span>
              回收前 {point.reclaimedFromMb.toFixed(2)} MB → 回收后{' '}
              {point.workingSetMb.toFixed(2)} MB ·{' '}
            </span>
          ) : null}
          <span>{point.note}</span>
        </div>
      )}
    </div>
  );
}

const STATE_FILTERS: Array<{ id: 'all' | 'idle' | 'active'; label: string }> = [
  { id: 'all', label: '全部采样时段' },
  { id: 'idle', label: '仅空闲驻留' },
  { id: 'active', label: '仅报时 / 回收态' },
];

export function SystemOverheadPanel({ theme }: SystemOverheadPanelProps) {
  const [profileId, setProfileId] = useState<HardwareProfileId>('low_spec_dual');
  const [stateFilter, setStateFilter] = useState<'all' | 'idle' | 'active'>('all');
  const [uploadedReport, setUploadedReport] = useState<SystemOverheadReport | null>(null);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  const isDark = theme.id === 'obsidian';
  const gridStroke = isDark ? 'rgba(148, 163, 184, 0.14)' : 'rgba(15, 23, 42, 0.08)';
  const sysCpuStroke = isDark ? 'rgba(148, 163, 184, 0.65)' : 'rgba(100, 116, 139, 0.72)';

  const report = useMemo(() => {
    if (uploadedReport) return uploadedReport;
    return buildProfileOverheadReport(profileId);
  }, [uploadedReport, profileId]);

  const activeProfileMeta = useMemo(
    () => HARDWARE_PROFILES.find((p) => p.id === profileId) ?? HARDWARE_PROFILES[0],
    [profileId],
  );

  const visibleSamples = useMemo(() => {
    if (stateFilter === 'idle') {
      return report.samples.filter((s) => s.state === 'idle');
    }
    if (stateFilter === 'active') {
      return report.samples.filter((s) => s.state !== 'idle');
    }
    return report.samples;
  }, [report.samples, stateFilter]);

  const handleUploadPerfLog = async (fileList: FileList | null) => {
    if (!fileList || fileList.length === 0) return;
    setUploadError(null);
    try {
      const files: Array<{ fileName: string; content: string }> = [];
      for (const f of Array.from(fileList)) {
        files.push({ fileName: f.name, content: await f.text() });
      }
      const parsed = parsePerfDebugLogs(files);
      if (!parsed) {
        setUploadError(
          '未在所选文件中检测到 [perf] 采样日志。请在 Windows 终端设置 HALF_HOUR_CHIME_DEBUG=1 后启动程序，再导入生成的 half_hour_chime_*.log。',
        );
        return;
      }
      setUploadedReport(parsed);
      setStateFilter('all');
    } catch (err) {
      setUploadError(err instanceof Error ? err.message : '读取调试日志文件失败');
    }
  };

  const handleExportJson = () => {
    const blob = new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `half_hour_chime_overhead_${profileId}.json`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
  };

  const verdictColor =
    report.lowSpecVerdict === 'optimal'
      ? '#16A34A'
      : report.lowSpecVerdict === 'guarded'
        ? '#D97706'
        : '#DC2626';

  return (
    <div className="space-y-6">
      {/* 顶部上下文与硬件场景选择器 */}
      <section
        className="rounded-xl border px-5 py-4"
        style={{ borderColor: theme.border, backgroundColor: theme.surface }}
      >
        <div className="flex flex-wrap items-center justify-between gap-4">
          <div className="min-w-0 space-y-1">
            <div className="flex items-center gap-2">
              <Gauge className="h-4 w-4 shrink-0" style={{ color: theme.accent }} />
              <h1 className="text-base font-bold tracking-tight">
                系统开销分析 · Rust 后端空闲与报时资源效率评估
              </h1>
            </div>
            <div
              className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs"
              style={{ color: theme.textMuted }}
            >
              <span>采集架构：Windows PDH 计数器 + PSAPI / GetProcessTimes 双路兜底</span>
              <span aria-hidden="true">·</span>
              <span className="font-mono tabular-nums">采样周期 30s</span>
              <span aria-hidden="true">·</span>
              <span className="font-mono tabular-nums">强制回收红线 50 MiB (120s 冷却)</span>
              <span aria-hidden="true">·</span>
              <span>{report.hardwareSpec}</span>
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2 shrink-0">
            <input
              ref={fileInputRef}
              type="file"
              multiple
              accept=".log,.txt,.1,.2,.3"
              className="hidden"
              onChange={(e) => {
                void handleUploadPerfLog(e.target.files);
                e.target.value = '';
              }}
            />
            <button
              type="button"
              onClick={() => fileInputRef.current?.click()}
              className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-lg px-3.5 py-2 text-xs font-semibold transition-colors duration-150 shrink-0"
              style={{ backgroundColor: theme.accent, color: theme.accentFg }}
            >
              <Upload className="h-3.5 w-3.5" />
              <span>导入实机 [perf] 日志</span>
            </button>
            <button
              type="button"
              onClick={handleExportJson}
              className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-lg border px-3 py-2 text-xs font-semibold transition-colors duration-150 shrink-0"
              style={{
                borderColor: theme.border,
                backgroundColor: theme.canvas,
                color: theme.textPrimary,
              }}
            >
              <Download className="h-3.5 w-3.5" />
              <span>导出评估报告</span>
            </button>
            {report.isUploadedLog && (
              <button
                type="button"
                onClick={() => {
                  setUploadedReport(null);
                  setUploadError(null);
                }}
                className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-lg border px-3 py-2 text-xs font-semibold transition-colors duration-150 shrink-0"
                style={{
                  borderColor: theme.border,
                  backgroundColor: theme.canvas,
                  color: theme.textSecondary,
                }}
              >
                <RotateCcw className="h-3.5 w-3.5" />
                <span>返回基准场景</span>
              </button>
            )}
          </div>
        </div>

        {/* 场景切换 & 状态过滤栏 */}
        <div
          className="mt-3.5 flex flex-wrap items-center justify-between gap-3 border-t pt-3.5"
          style={{ borderColor: theme.border }}
        >
          <div className="flex flex-wrap items-center gap-1.5">
            {HARDWARE_PROFILES.map((prof) => {
              const active = !report.isUploadedLog && profileId === prof.id;
              return (
                <button
                  key={prof.id}
                  type="button"
                  onClick={() => {
                    setUploadedReport(null);
                    setUploadError(null);
                    setProfileId(prof.id);
                  }}
                  className="cursor-pointer whitespace-nowrap rounded-lg border px-3 py-1.5 text-xs font-semibold transition-colors duration-150 shrink-0"
                  style={{
                    borderColor: active ? theme.accent : theme.border,
                    backgroundColor: active ? theme.accent : theme.canvas,
                    color: active ? theme.accentFg : theme.textSecondary,
                  }}
                >
                  {prof.shortLabel}
                </button>
              );
            })}
          </div>

          <div
            className="flex flex-wrap items-center gap-1 rounded-lg border p-1"
            style={{ borderColor: theme.border, backgroundColor: theme.canvas }}
          >
            {STATE_FILTERS.map((f) => {
              const active = stateFilter === f.id;
              return (
                <button
                  key={f.id}
                  type="button"
                  onClick={() => setStateFilter(f.id)}
                  className="cursor-pointer whitespace-nowrap rounded-md px-2.5 py-1 text-xs font-medium transition-colors duration-150 shrink-0"
                  style={{
                    backgroundColor: active ? theme.accent : 'transparent',
                    color: active ? theme.accentFg : theme.textSecondary,
                  }}
                >
                  {f.label}
                </button>
              );
            })}
          </div>
        </div>

        {!report.isUploadedLog && (
          <p className="mt-2.5 text-xs" style={{ color: theme.textSecondary }}>
            {activeProfileMeta.description}
          </p>
        )}

        {uploadError && (
          <div
            className="mt-3 flex items-start justify-between gap-3 border-t pt-3 text-xs"
            style={{ borderColor: theme.border, color: '#DC2626' }}
          >
            <div className="flex items-start gap-2">
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              <span>{uploadError}</span>
            </div>
            <button
              type="button"
              onClick={() => setUploadError(null)}
              className="cursor-pointer p-0.5 opacity-80 hover:opacity-100"
              aria-label="关闭错误提示"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        )}
      </section>

      {/* 4 栏资源效率对比 KPI Strip */}
      <section className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <div
          className="rounded-xl border px-4 py-3.5"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium" style={{ color: theme.textMuted }}>
              空闲托盘态均值
            </span>
            <MemoryStick className="h-3.5 w-3.5 shrink-0" style={{ color: theme.accent }} />
          </div>
          <div className="mt-1.5 flex items-baseline gap-2.5">
            <span className="font-mono text-2xl font-bold tabular-nums">
              {report.idleAvgWorkingSetMb.toFixed(1)} MB
            </span>
            <span className="font-mono text-xs tabular-nums" style={{ color: '#16A34A' }}>
              CPU {report.idleAvgCpuPct.toFixed(2)}%
            </span>
          </div>
          <div className="mt-1 text-xs tabular-nums" style={{ color: theme.textSecondary }}>
            私有提交均值 {report.idleAvgPrivateMb.toFixed(1)} MB · 隐藏主窗零字符串分配
          </div>
        </div>

        <div
          className="rounded-xl border px-4 py-3.5"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium" style={{ color: theme.textMuted }}>
              报时播报态峰值
            </span>
            <Cpu className="h-3.5 w-3.5 shrink-0" style={{ color: theme.accent }} />
          </div>
          <div className="mt-1.5 flex items-baseline gap-2.5">
            <span className="font-mono text-2xl font-bold tabular-nums">
              {report.chimePeakWorkingSetMb.toFixed(1)} MB
            </span>
            <span className="font-mono text-xs tabular-nums" style={{ color: '#2563EB' }}>
              CPU 峰值 {report.chimePeakCpuPct.toFixed(2)}%
            </span>
          </div>
          <div className="mt-1 text-xs tabular-nums" style={{ color: theme.textSecondary }}>
            播报均值 {report.chimeAvgWorkingSetMb.toFixed(1)} MB · CPU 均值{' '}
            {report.chimeAvgCpuPct.toFixed(2)}%
          </div>
        </div>

        <div
          className="rounded-xl border px-4 py-3.5"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="text-xs font-medium" style={{ color: theme.textMuted }}>
            50 MiB 守护与强制回收
          </div>
          <div className="mt-1.5 flex items-baseline gap-2.5">
            <span className="font-mono text-2xl font-bold tabular-nums">
              {report.gcReclaimCount} 次
            </span>
            <span className="font-mono text-xs tabular-nums" style={{ color: theme.textMuted }}>
              {report.maxReclaimedMb > 0
                ? `单次释放 ${report.maxReclaimedMb.toFixed(1)} MB`
                : `余量 ${report.headroomTo50MbPct.toFixed(1)}%`}
            </span>
          </div>
          <div className="mt-1 text-xs" style={{ color: theme.textSecondary }}>
            超限即执行 HeapCompact + EmptyWorkingSet
          </div>
        </div>

        <div
          className="rounded-xl border px-4 py-3.5"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium" style={{ color: theme.textMuted }}>
              低配 PC 运行效率评估
            </span>
            <ShieldCheck className="h-3.5 w-3.5 shrink-0" style={{ color: verdictColor }} />
          </div>
          <div className="mt-1.5 flex items-baseline gap-2">
            <span className="text-lg font-bold tracking-tight" style={{ color: verdictColor }}>
              {report.lowSpecVerdictLabel}
            </span>
          </div>
          <div className="mt-1 text-xs tabular-nums" style={{ color: theme.textSecondary }}>
            {report.cpuThrottleCount > 0
              ? `高负载让步 ${report.cpuThrottleCount} 个采样周期（已推迟非核心请求）`
              : '进程 CPU 远低于 15% 警戒线，可直接部署老旧客户端'}
          </div>
        </div>
      </section>

      {/* 双趋势图表：内存占用 + CPU 使用率 */}
      {visibleSamples.length === 0 ? (
        <section
          className="flex flex-col items-center justify-center gap-2 rounded-xl border px-6 py-12 text-center"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <Gauge className="h-7 w-7" style={{ color: theme.textMuted }} />
          <div className="text-sm font-semibold">当前筛选状态下暂无采样点</div>
          <p className="max-w-md text-xs" style={{ color: theme.textMuted }}>
            所加载的数据集中不包含当前筛选状态（{STATE_FILTERS.find((f) => f.id === stateFilter)?.label}）的采样记录。
          </p>
          <button
            type="button"
            onClick={() => setStateFilter('all')}
            className="mt-1 cursor-pointer whitespace-nowrap rounded-lg border px-3.5 py-1.5 text-xs font-semibold"
            style={{
              borderColor: theme.border,
              backgroundColor: theme.canvas,
              color: theme.textPrimary,
            }}
          >
            查看全部采样时段
          </button>
        </section>
      ) : (
        <section className="grid grid-cols-1 gap-6 lg:grid-cols-12">
          {/* 左图：空闲 vs 报时内存占用趋势 */}
          <div
            className="rounded-xl border p-5 lg:col-span-6"
            style={{ borderColor: theme.border, backgroundColor: theme.surface }}
          >
            <div className="mb-4">
              <h2 className="text-sm font-bold">空闲与报时态内存占用趋势（MB）</h2>
              <p className="mt-0.5 text-xs" style={{ color: theme.textMuted }}>
                对比工作集（Working Set）与私有提交内存，虚线为 perf_guard 50 MiB 自动回收红线
              </p>
            </div>

            <div className="h-72 w-full">
              <ResponsiveContainer width="100%" height="100%">
                <ComposedChart
                  data={visibleSamples}
                  margin={{ top: 10, right: 12, left: -12, bottom: 0 }}
                >
                  <CartesianGrid stroke={gridStroke} strokeDasharray="3 3" vertical={false} />
                  <XAxis
                    dataKey="timeLabel"
                    tick={{ fill: theme.textMuted, fontSize: 11 }}
                    axisLine={{ stroke: theme.border }}
                    tickLine={false}
                  />
                  <YAxis
                    domain={[0, (max: number) => Math.max(60, Math.ceil(max / 10) * 10)]}
                    tickFormatter={(v: number) => `${v}M`}
                    tick={{ fill: theme.textMuted, fontSize: 11 }}
                    axisLine={false}
                    tickLine={false}
                  />
                  <ReferenceLine
                    y={50}
                    stroke="#DC2626"
                    strokeDasharray="4 4"
                    label={{
                      value: '50 MiB 强制回收阈值',
                      position: 'insideTopRight',
                      fill: '#DC2626',
                      fontSize: 11,
                    }}
                  />
                  <Tooltip content={<OverheadChartTooltip theme={theme} mode="memory" />} />
                  <Legend
                    wrapperStyle={{ fontSize: '12px', paddingTop: '8px', color: theme.textSecondary }}
                  />
                  <Area
                    type="monotone"
                    dataKey="workingSetMb"
                    name="工作集内存 (MB)"
                    stroke={theme.accent}
                    fill={theme.accent}
                    fillOpacity={0.18}
                    strokeWidth={2.2}
                  />
                  <Line
                    type="monotone"
                    dataKey="privateMb"
                    name="私有提交内存 (MB)"
                    stroke="#2563EB"
                    strokeWidth={2}
                    dot={{ r: 3, fill: '#2563EB', strokeWidth: 0 }}
                  />
                </ComposedChart>
              </ResponsiveContainer>
            </div>
          </div>

          {/* 右图：本进程 CPU vs 整机 CPU 使用率趋势 */}
          <div
            className="rounded-xl border p-5 lg:col-span-6"
            style={{ borderColor: theme.border, backgroundColor: theme.surface }}
          >
            <div className="mb-4">
              <h2 className="text-sm font-bold">空闲与报时态 CPU 使用率趋势（%）</h2>
              <p className="mt-0.5 text-xs" style={{ color: theme.textMuted }}>
                左轴为 HalfHourChime 进程占用，右轴为整机总 CPU；整机 ≥ 85% 时自动推迟后台联网任务
              </p>
            </div>

            <div className="h-72 w-full">
              <ResponsiveContainer width="100%" height="100%">
                <ComposedChart
                  data={visibleSamples}
                  margin={{ top: 10, right: 8, left: -12, bottom: 0 }}
                >
                  <CartesianGrid stroke={gridStroke} strokeDasharray="3 3" vertical={false} />
                  <XAxis
                    dataKey="timeLabel"
                    tick={{ fill: theme.textMuted, fontSize: 11 }}
                    axisLine={{ stroke: theme.border }}
                    tickLine={false}
                  />
                  <YAxis
                    yAxisId="proc"
                    domain={[0, (max: number) => Math.max(18, Math.ceil(max * 1.2))]}
                    tickFormatter={(v: number) => `${v}%`}
                    tick={{ fill: theme.accent, fontSize: 11 }}
                    axisLine={false}
                    tickLine={false}
                  />
                  <YAxis
                    yAxisId="sys"
                    orientation="right"
                    domain={[0, 100]}
                    tickFormatter={(v: number) => `${v}%`}
                    tick={{ fill: theme.textMuted, fontSize: 11 }}
                    axisLine={false}
                    tickLine={false}
                  />
                  <ReferenceLine
                    yAxisId="proc"
                    y={15}
                    stroke="#D97706"
                    strokeDasharray="4 4"
                    label={{
                      value: '进程告警线 15%',
                      position: 'insideTopLeft',
                      fill: '#D97706',
                      fontSize: 11,
                    }}
                  />
                  <ReferenceLine
                    yAxisId="sys"
                    y={85}
                    stroke="#DC2626"
                    strokeDasharray="3 3"
                    label={{
                      value: '整机让步线 85%',
                      position: 'insideTopRight',
                      fill: '#DC2626',
                      fontSize: 11,
                    }}
                  />
                  <Tooltip content={<OverheadChartTooltip theme={theme} mode="cpu" />} />
                  <Legend
                    wrapperStyle={{ fontSize: '12px', paddingTop: '8px', color: theme.textSecondary }}
                  />
                  <Area
                    yAxisId="proc"
                    type="monotone"
                    dataKey="processCpuPct"
                    name="Rust 进程 CPU (%)"
                    stroke={theme.accent}
                    fill={theme.accent}
                    fillOpacity={0.2}
                    strokeWidth={2.2}
                  />
                  <Line
                    yAxisId="sys"
                    type="monotone"
                    dataKey="systemCpuPct"
                    name="整机 CPU 负载 (%)"
                    stroke={sysCpuStroke}
                    strokeWidth={1.8}
                    strokeDasharray="4 3"
                    dot={false}
                  />
                </ComposedChart>
              </ResponsiveContainer>
            </div>
          </div>
        </section>
      )}

      {/* 空闲 vs 报时运行状态分项对比表 */}
      <section
        className="overflow-hidden rounded-xl border"
        style={{ borderColor: theme.border, backgroundColor: theme.surface }}
      >
        <div
          className="flex flex-wrap items-center justify-between gap-2 border-b px-5 py-3.5"
          style={{ borderColor: theme.border }}
        >
          <div>
            <h2 className="text-sm font-bold">运行状态资源开销对比与 Rust 内核优化机制</h2>
            <p className="mt-0.5 text-xs" style={{ color: theme.textMuted }}>
              直观对比静默驻留与触发音频/TTS 播报阶段的资源开销，辅助低配网吧客户端容量规划
            </p>
          </div>
          <span className="font-mono text-xs tabular-nums" style={{ color: theme.textMuted }}>
            {visibleSamples.length} / {report.samples.length} 个采样点
          </span>
        </div>

        <div className="overflow-x-auto">
          <table className="w-full border-collapse text-left text-xs">
            <thead>
              <tr
                className="border-b font-medium"
                style={{ borderColor: theme.border, color: theme.textMuted }}
              >
                <th className="whitespace-nowrap px-5 py-2.5">运行状态</th>
                <th className="whitespace-nowrap px-4 py-2.5 text-right">采样数</th>
                <th className="whitespace-nowrap px-4 py-2.5 text-right">工作集均值</th>
                <th className="whitespace-nowrap px-4 py-2.5 text-right">工作集峰值</th>
                <th className="whitespace-nowrap px-4 py-2.5 text-right">私有内存均值</th>
                <th className="whitespace-nowrap px-4 py-2.5 text-right">进程 CPU 均值</th>
                <th className="whitespace-nowrap px-4 py-2.5 text-right">进程 CPU 峰值</th>
                <th className="px-5 py-2.5">底层触发与资源管控逻辑</th>
              </tr>
            </thead>
            <tbody>
              {report.stateBreakdowns.map((row) => {
                const color = STATE_META[row.state as PerfStateKind].color;
                return (
                  <tr
                    key={row.state}
                    className="border-b last:border-b-0"
                    style={{ borderColor: theme.border }}
                  >
                    <td className="whitespace-nowrap px-5 py-2.5 font-semibold" style={{ color }}>
                      {row.label}
                    </td>
                    <td
                      className="whitespace-nowrap px-4 py-2.5 text-right font-mono tabular-nums"
                      style={{ color: theme.textSecondary }}
                    >
                      {row.sampleCount}
                    </td>
                    <td className="whitespace-nowrap px-4 py-2.5 text-right font-mono tabular-nums font-semibold">
                      {row.avgWorkingSetMb.toFixed(2)} MB
                    </td>
                    <td
                      className="whitespace-nowrap px-4 py-2.5 text-right font-mono tabular-nums"
                      style={{ color: theme.textSecondary }}
                    >
                      {row.peakWorkingSetMb.toFixed(2)} MB
                    </td>
                    <td
                      className="whitespace-nowrap px-4 py-2.5 text-right font-mono tabular-nums"
                      style={{ color: theme.textSecondary }}
                    >
                      {row.avgPrivateMb.toFixed(2)} MB
                    </td>
                    <td className="whitespace-nowrap px-4 py-2.5 text-right font-mono tabular-nums font-semibold">
                      {row.avgProcCpuPct.toFixed(2)}%
                    </td>
                    <td
                      className="whitespace-nowrap px-4 py-2.5 text-right font-mono tabular-nums"
                      style={{ color: theme.textSecondary }}
                    >
                      {row.peakProcCpuPct.toFixed(2)}%
                    </td>
                    <td className="px-5 py-2.5" style={{ color: theme.textSecondary }}>
                      {row.mechanismNote}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  );
}
