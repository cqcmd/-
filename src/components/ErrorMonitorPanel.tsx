import { useMemo, useRef, useState } from 'react';
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  ClipboardPaste,
  Download,
  FileWarning,
  FolderArchive,
  RotateCcw,
  Search,
  Upload,
  X,
} from 'lucide-react';
import {
  Bar,
  BarChart,
  CartesianGrid,
  ComposedChart,
  Legend,
  Line,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import {
  AudioExceptionKind,
  buildSampleSevenDayLogs,
  DailyMonitorMetric,
  EXCEPTION_KIND_META,
  parseRotatingLogs,
  RawLogFileInput,
} from '../utils/logMonitorParser';

export interface MonitorThemeTokens {
  id: 'obsidian' | 'win11' | 'warm';
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

interface ErrorMonitorPanelProps {
  theme: MonitorThemeTokens;
}

interface ChartTooltipPayloadItem {
  name?: string;
  value?: number | string;
  color?: string;
  dataKey?: string;
  payload?: DailyMonitorMetric;
}

interface CustomTooltipProps {
  active?: boolean;
  payload?: ChartTooltipPayloadItem[];
  label?: string;
  theme: MonitorThemeTokens;
  mode: 'rate' | 'audio';
}

function CustomChartTooltip({ active, payload, label, theme, mode }: CustomTooltipProps) {
  if (!active || !payload || payload.length === 0) return null;
  const row = payload[0]?.payload;

  return (
    <div
      className="rounded-lg border px-3.5 py-2.5 shadow-sm text-xs"
      style={{
        backgroundColor: theme.surface,
        borderColor: theme.border,
        color: theme.textPrimary,
      }}
    >
      <div className="mb-1.5 flex items-center justify-between gap-4 font-mono font-semibold">
        <span>{row?.date ?? label}</span>
        {row && row.splitRolloverCount > 0 && (
          <span style={{ color: '#D97706' }}>触发同日分片 {row.splitRolloverCount} 次</span>
        )}
      </div>
      <div className="space-y-1">
        {payload.map((item) => {
          const isRate = item.dataKey === 'failureRatePct';
          const formattedVal =
            typeof item.value === 'number'
              ? isRate
                ? `${item.value.toFixed(2)}%`
                : `${item.value} 次`
              : String(item.value ?? '0');

          return (
            <div key={String(item.dataKey ?? item.name)} className="flex items-center justify-between gap-5">
              <span className="flex items-center gap-1.5" style={{ color: theme.textSecondary }}>
                <span
                  className="inline-block h-2 w-2 shrink-0 rounded-xs"
                  style={{ backgroundColor: item.color ?? theme.accent }}
                />
                <span>{item.name}</span>
              </span>
              <span className="font-mono tabular-nums font-semibold">{formattedVal}</span>
            </div>
          );
        })}
      </div>
      {row && mode === 'rate' && (
        <div
          className="mt-2 border-t pt-1.5 text-[11px]"
          style={{ borderColor: theme.border, color: theme.textMuted }}
        >
          归档文件：{row.archiveFiles.length > 0 ? row.archiveFiles.join(', ') : '当日无归档记录'}
        </div>
      )}
    </div>
  );
}

const EXCEPTION_FILTERS: Array<{ id: 'all' | AudioExceptionKind; label: string }> = [
  { id: 'all', label: '全部异常' },
  { id: 'file_missing', label: '文件缺失' },
  { id: 'decode_error', label: '解码失败' },
  { id: 'tts_fallback', label: 'TTS 回退' },
  { id: 'device_stream', label: '设备与流' },
];

export function ErrorMonitorPanel({ theme }: ErrorMonitorPanelProps) {
  const defaultSampleFiles = useMemo(() => buildSampleSevenDayLogs(), []);
  const [customFiles, setCustomFiles] = useState<RawLogFileInput[] | null>(null);
  const [sourceTag, setSourceTag] = useState<string>(
    '基准归档数据集（基于 windows-native/src/win_sys.rs 按天归档策略生成）',
  );
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [showPasteBox, setShowPasteBox] = useState(false);
  const [pasteText, setPasteText] = useState('');

  const [selectedDay, setSelectedDay] = useState<string>('all');
  const [selectedKind, setSelectedKind] = useState<'all' | AudioExceptionKind>('all');
  const [searchQuery, setSearchQuery] = useState('');

  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const isDark = theme.id === 'obsidian';
  const gridStroke = isDark ? 'rgba(148, 163, 184, 0.14)' : 'rgba(15, 23, 42, 0.08)';
  const neutralBarColor = isDark ? 'rgba(148, 163, 184, 0.28)' : 'rgba(100, 116, 139, 0.24)';

  const report = useMemo(() => {
    const activeFiles = customFiles ?? defaultSampleFiles;
    return parseRotatingLogs(activeFiles, {
      sourceLabel: sourceTag,
      isSampleData: customFiles === null,
    });
  }, [customFiles, defaultSampleFiles, sourceTag]);

  const filteredAnomalies = useMemo(() => {
    const q = searchQuery.trim().toLowerCase();
    return report.anomalies.filter((item) => {
      if (selectedDay !== 'all' && item.date !== selectedDay) return false;
      if (selectedKind !== 'all' && item.exceptionKind !== selectedKind) return false;
      if (q) {
        const hay = `${item.timestamp} ${item.target} ${item.message} ${item.sourceFile}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  }, [report.anomalies, selectedDay, selectedKind, searchQuery]);

  const handleFilesSelected = async (fileList: FileList | null) => {
    if (!fileList || fileList.length === 0) return;
    setUploadError(null);
    try {
      const loaded: RawLogFileInput[] = [];
      for (const file of Array.from(fileList)) {
        const text = await file.text();
        loaded.push({ fileName: file.name, content: text });
      }
      const probe = parseRotatingLogs(loaded);
      if (probe.totalLinesParsed === 0) {
        setUploadError(
          '所选文件未检测到符合 RotatingLogger 格式（[YYYY-MM-DD HH:MM:SS.mmm] [LEVEL] ...）的日志行，请确认导入的是 half_hour_chime_YYYY-MM-DD.log 文件。',
        );
        return;
      }
      setCustomFiles(loaded);
      setSourceTag(`本地导入 ${loaded.length} 个日志文件`);
      setSelectedDay('all');
      setSelectedKind('all');
    } catch (err) {
      setUploadError(err instanceof Error ? err.message : '读取本地日志文件失败');
    }
  };

  const handleParsePaste = () => {
    setUploadError(null);
    const trimmed = pasteText.trim();
    if (!trimmed) {
      setUploadError('请先粘贴至少一行 half_hour_chime_*.log 日志内容。');
      return;
    }
    const input = [{ fileName: 'pasted_archive.log', content: trimmed }];
    const probe = parseRotatingLogs(input);
    if (probe.totalLinesParsed === 0) {
      setUploadError(
        '粘贴内容未匹配到 RotatingLogger 标准日志行，示例：[2026-11-16 12:00:00.112] [WARN ] [pid:4120] [audio-worker] [audio] ...',
      );
      return;
    }
    setCustomFiles(input);
    setSourceTag('已解析手动粘贴的归档日志文本');
    setSelectedDay('all');
    setSelectedKind('all');
    setShowPasteBox(false);
  };

  const handleResetToSample = () => {
    setCustomFiles(null);
    setUploadError(null);
    setSourceTag('基准归档数据集（基于 windows-native/src/win_sys.rs 按天归档策略生成）');
    setSelectedDay('all');
    setSelectedKind('all');
    setSearchQuery('');
  };

  const handleExportReportJson = () => {
    const blob = new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `half_hour_chime_error_report_${report.dailyMetrics[6]?.date ?? '7d'}.json`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
  };

  const handleChartClick = (state: unknown) => {
    const maybeState = state as { activePayload?: Array<{ payload?: DailyMonitorMetric }> } | null;
    const clickedDate = maybeState?.activePayload?.[0]?.payload?.date;
    if (clickedDate) {
      setSelectedDay((prev) => (prev === clickedDate ? 'all' : clickedDate));
    }
  };

  const failureStatusColor =
    report.overallFailureRatePct === 0
      ? '#16A34A'
      : report.overallFailureRatePct <= 5
        ? '#D97706'
        : '#DC2626';
  const failureStatusLabel =
    report.overallFailureRatePct === 0
      ? '运行正常'
      : report.overallFailureRatePct <= 5
        ? '轻度降级告警'
        : '异常偏高需排查';

  return (
    <div className="space-y-6">
      {/* 顶部控制栏：按天归档策略上下文与本地 .log 导入 */}
      <section
        className="rounded-xl border px-5 py-4"
        style={{ borderColor: theme.border, backgroundColor: theme.surface }}
      >
        <div className="flex flex-wrap items-center justify-between gap-4">
          <div className="min-w-0 space-y-1">
            <div className="flex items-center gap-2">
              <Activity className="h-4 w-4 shrink-0" style={{ color: theme.accent }} />
              <h1 className="text-base font-bold tracking-tight">
                错误监控报告 · 近 7 天报时失败率与音轨异常分析
              </h1>
            </div>
            <div
              className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs"
              style={{ color: theme.textMuted }}
            >
              <span className="font-mono">half_hour_chime_YYYY-MM-DD.log</span>
              <span aria-hidden="true">·</span>
              <span>按天归档（保留 14 天，单日满 2 MiB 分片 .1~.3）</span>
              <span aria-hidden="true">·</span>
              <span>统计区间 {report.dateRangeLabel}</span>
              <span aria-hidden="true">·</span>
              <span>{report.sourceLabel}</span>
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
                void handleFilesSelected(e.target.files);
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
              <span>导入 dist\logs\*.log</span>
            </button>
            <button
              type="button"
              onClick={() => setShowPasteBox((v) => !v)}
              className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-lg border px-3 py-2 text-xs font-semibold transition-colors duration-150 shrink-0"
              style={{
                borderColor: theme.border,
                backgroundColor: theme.canvas,
                color: theme.textPrimary,
              }}
            >
              <ClipboardPaste className="h-3.5 w-3.5" />
              <span>粘贴日志</span>
            </button>
            <button
              type="button"
              onClick={handleExportReportJson}
              className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-lg border px-3 py-2 text-xs font-semibold transition-colors duration-150 shrink-0"
              style={{
                borderColor: theme.border,
                backgroundColor: theme.canvas,
                color: theme.textPrimary,
              }}
            >
              <Download className="h-3.5 w-3.5" />
              <span>导出报告 JSON</span>
            </button>
            {!report.isSampleData && (
              <button
                type="button"
                onClick={handleResetToSample}
                className="flex cursor-pointer items-center gap-1.5 whitespace-nowrap rounded-lg border px-3 py-2 text-xs font-semibold transition-colors duration-150 shrink-0"
                style={{
                  borderColor: theme.border,
                  backgroundColor: theme.canvas,
                  color: theme.textSecondary,
                }}
              >
                <RotateCcw className="h-3.5 w-3.5" />
                <span>恢复基准样本</span>
              </button>
            )}
          </div>
        </div>

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

        {showPasteBox && (
          <div className="mt-3 space-y-2 border-t pt-3" style={{ borderColor: theme.border }}>
            <div className="flex items-center justify-between text-xs" style={{ color: theme.textSecondary }}>
              <span>粘贴来自 dist\logs\half_hour_chime_YYYY-MM-DD.log 的文本内容进行实时解析：</span>
              <button
                type="button"
                onClick={() => setShowPasteBox(false)}
                className="cursor-pointer text-xs underline-offset-2 hover:underline"
                style={{ color: theme.textMuted }}
              >
                收起
              </button>
            </div>
            <textarea
              rows={5}
              value={pasteText}
              onChange={(e) => setPasteText(e.target.value)}
              placeholder="[2026-11-16 12:00:00.112] [WARN ] [pid:4120] [audio-worker] [audio] open custom meal audio &quot;D:\Media\meal.mp3&quot; failed: ..."
              className="w-full rounded-lg border p-3 font-mono text-xs outline-none focus:ring-1"
              style={{
                borderColor: theme.border,
                backgroundColor: theme.codeBg,
                color: theme.textPrimary,
              }}
            />
            <div className="flex justify-end gap-2">
              <button
                type="button"
                onClick={handleParsePaste}
                className="cursor-pointer whitespace-nowrap rounded-lg px-4 py-1.5 text-xs font-semibold"
                style={{ backgroundColor: theme.accent, color: theme.accentFg }}
              >
                立即解析
              </button>
            </div>
          </div>
        )}
      </section>

      {/* 4 栏关键指标 KPI 条（单层平铺 1px 描边，等宽数字 tabular-nums） */}
      <section className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <div
          className="rounded-xl border px-4 py-3.5"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="text-xs font-medium" style={{ color: theme.textMuted }}>
            近 7 天调度报时总次
          </div>
          <div className="mt-1.5 flex items-baseline gap-2">
            <span className="font-mono text-2xl font-bold tabular-nums">{report.totalChimes7d}</span>
            <span className="text-xs" style={{ color: theme.textMuted }}>
              次
            </span>
          </div>
          <div className="mt-1 text-xs tabular-nums" style={{ color: theme.textSecondary }}>
            日均 {Math.round(report.totalChimes7d / 7)} 次 · 共解析 {report.totalLinesParsed} 行日志
          </div>
        </div>

        <div
          className="rounded-xl border px-4 py-3.5"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium" style={{ color: theme.textMuted }}>
              近 7 天报时失败率
            </span>
            <span className="text-xs font-semibold" style={{ color: failureStatusColor }}>
              {failureStatusLabel}
            </span>
          </div>
          <div className="mt-1.5 flex items-baseline gap-2">
            <span
              className="font-mono text-2xl font-bold tabular-nums"
              style={{ color: failureStatusColor }}
            >
              {report.overallFailureRatePct.toFixed(2)}%
            </span>
            <span className="font-mono text-xs tabular-nums" style={{ color: theme.textMuted }}>
              ({report.totalFailedChimes7d} 次异常)
            </span>
          </div>
          <div className="mt-1 text-xs tabular-nums" style={{ color: theme.textSecondary }}>
            {report.peakFailureDay
              ? `峰值日 ${report.peakFailureDay.shortDate} · ${report.peakFailureDay.failureRatePct.toFixed(2)}%`
              : '近 7 天无报时失败记录'}
          </div>
        </div>

        <div
          className="rounded-xl border px-4 py-3.5"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="text-xs font-medium" style={{ color: theme.textMuted }}>
            音轨与语音加载异常
          </div>
          <div className="mt-1.5 flex items-baseline gap-2">
            <span className="font-mono text-2xl font-bold tabular-nums">
              {report.totalAudioExceptions7d}
            </span>
            <span className="text-xs" style={{ color: theme.textMuted }}>
              次
            </span>
          </div>
          <div className="mt-1 truncate text-xs" style={{ color: theme.textSecondary }}>
            {report.topExceptionCategory
              ? `主因：${report.topExceptionCategory.label} (${report.topExceptionCategory.count})`
              : '未检测到音轨加载或解码异常'}
          </div>
        </div>

        <div
          className="rounded-xl border px-4 py-3.5"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs font-medium" style={{ color: theme.textMuted }}>
              日志按天归档健康度
            </span>
            <FolderArchive className="h-3.5 w-3.5 shrink-0" style={{ color: theme.accent }} />
          </div>
          <div className="mt-1.5 flex items-baseline gap-2">
            <span className="font-mono text-2xl font-bold tabular-nums">
              {report.totalFilesAnalyzed}
            </span>
            <span className="text-xs" style={{ color: theme.textMuted }}>
              个归档文件
            </span>
          </div>
          <div className="mt-1 text-xs tabular-nums" style={{ color: theme.textSecondary }}>
            {report.totalSplitRollovers > 0
              ? `检测到 ${report.totalSplitRollovers} 次单日超 2 MiB 自动分片`
              : '未触发 2 MiB 单日超限分片 · 14 天保留正常'}
          </div>
        </div>
      </section>

      {/* Recharts 双图表主体区 */}
      <section className="grid grid-cols-1 gap-6 lg:grid-cols-12">
        {/* 图表 1：近 7 天报时失败率趋势 */}
        <div
          className="rounded-xl border p-5 lg:col-span-6"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="mb-4 flex flex-wrap items-start justify-between gap-2">
            <div>
              <h2 className="text-sm font-bold">近 7 天报时执行量与失败率趋势</h2>
              <p className="mt-0.5 text-xs" style={{ color: theme.textMuted }}>
                失败率 = 报时异常次数 / 当日调度报时总次数（点击柱状图可下钻该日明细）
              </p>
            </div>
            {selectedDay !== 'all' && (
              <button
                type="button"
                onClick={() => setSelectedDay('all')}
                className="cursor-pointer whitespace-nowrap text-xs font-medium underline-offset-2 hover:underline"
                style={{ color: theme.accent }}
              >
                清除日期筛选 ({selectedDay.slice(5)})
              </button>
            )}
          </div>

          <div className="h-72 w-full">
            <ResponsiveContainer width="100%" height="100%">
              <ComposedChart
                data={report.dailyMetrics}
                margin={{ top: 8, right: 8, left: -12, bottom: 0 }}
                onClick={handleChartClick}
              >
                <CartesianGrid stroke={gridStroke} strokeDasharray="3 3" vertical={false} />
                <XAxis
                  dataKey="shortDate"
                  tick={{ fill: theme.textMuted, fontSize: 11 }}
                  axisLine={{ stroke: theme.border }}
                  tickLine={false}
                />
                <YAxis
                  yAxisId="count"
                  allowDecimals={false}
                  tick={{ fill: theme.textMuted, fontSize: 11 }}
                  axisLine={false}
                  tickLine={false}
                />
                <YAxis
                  yAxisId="rate"
                  orientation="right"
                  domain={[0, (max: number) => Math.max(10, Math.ceil(max * 1.25))]}
                  tickFormatter={(v: number) => `${v}%`}
                  tick={{ fill: '#DC2626', fontSize: 11 }}
                  axisLine={false}
                  tickLine={false}
                />
                <Tooltip content={<CustomChartTooltip theme={theme} mode="rate" />} />
                <Legend
                  wrapperStyle={{ fontSize: '12px', paddingTop: '8px', color: theme.textSecondary }}
                />
                <Bar
                  yAxisId="count"
                  dataKey="totalChimes"
                  name="调度报时总次"
                  fill={neutralBarColor}
                  radius={[4, 4, 0, 0]}
                  barSize={20}
                  className="cursor-pointer"
                />
                <Bar
                  yAxisId="count"
                  dataKey="failedChimes"
                  name="报时失败次数"
                  fill="#EA580C"
                  radius={[4, 4, 0, 0]}
                  barSize={14}
                  className="cursor-pointer"
                />
                <Line
                  yAxisId="rate"
                  type="monotone"
                  dataKey="failureRatePct"
                  name="报时失败率 (%)"
                  stroke="#DC2626"
                  strokeWidth={2.5}
                  dot={{ r: 4, fill: '#DC2626', strokeWidth: 0 }}
                  activeDot={{ r: 6 }}
                />
              </ComposedChart>
            </ResponsiveContainer>
          </div>
        </div>

        {/* 图表 2：近 7 天音轨加载与语音异常分类堆叠柱状图 */}
        <div
          className="rounded-xl border p-5 lg:col-span-6"
          style={{ borderColor: theme.border, backgroundColor: theme.surface }}
        >
          <div className="mb-4 flex flex-wrap items-start justify-between gap-2">
            <div>
              <h2 className="text-sm font-bold">近 7 天音轨加载与合成异常统计</h2>
              <p className="mt-0.5 text-xs" style={{ color: theme.textMuted }}>
                按 audio.rs / tts 日志分类聚合：文件缺失、解码失败、Edge TTS 回退与 WASAPI 流异常
              </p>
            </div>
          </div>

          <div className="h-72 w-full">
            <ResponsiveContainer width="100%" height="100%">
              <BarChart
                data={report.dailyMetrics}
                margin={{ top: 8, right: 8, left: -12, bottom: 0 }}
                onClick={handleChartClick}
              >
                <CartesianGrid stroke={gridStroke} strokeDasharray="3 3" vertical={false} />
                <XAxis
                  dataKey="shortDate"
                  tick={{ fill: theme.textMuted, fontSize: 11 }}
                  axisLine={{ stroke: theme.border }}
                  tickLine={false}
                />
                <YAxis
                  allowDecimals={false}
                  tick={{ fill: theme.textMuted, fontSize: 11 }}
                  axisLine={false}
                  tickLine={false}
                />
                <Tooltip content={<CustomChartTooltip theme={theme} mode="audio" />} />
                <Legend
                  wrapperStyle={{ fontSize: '12px', paddingTop: '8px', color: theme.textSecondary }}
                />
                <Bar
                  dataKey="fileMissingCount"
                  name="音频文件缺失"
                  stackId="audio"
                  fill={EXCEPTION_KIND_META.file_missing.color}
                  barSize={24}
                  className="cursor-pointer"
                />
                <Bar
                  dataKey="decodeErrorCount"
                  name="音轨解码失败"
                  stackId="audio"
                  fill={EXCEPTION_KIND_META.decode_error.color}
                  barSize={24}
                  className="cursor-pointer"
                />
                <Bar
                  dataKey="ttsFallbackCount"
                  name="Edge TTS 回退"
                  stackId="audio"
                  fill={EXCEPTION_KIND_META.tts_fallback.color}
                  barSize={24}
                  className="cursor-pointer"
                />
                <Bar
                  dataKey="deviceStreamCount"
                  name="设备与流异常"
                  stackId="audio"
                  fill={EXCEPTION_KIND_META.device_stream.color}
                  radius={[4, 4, 0, 0]}
                  barSize={24}
                  className="cursor-pointer"
                />
              </BarChart>
            </ResponsiveContainer>
          </div>
        </div>
      </section>

      {/* 下钻异常日志明细表 */}
      <section
        className="overflow-hidden rounded-xl border"
        style={{ borderColor: theme.border, backgroundColor: theme.surface }}
      >
        <div
          className="flex flex-wrap items-center justify-between gap-3 border-b px-5 py-3.5"
          style={{ borderColor: theme.border }}
        >
          <div className="flex items-center gap-2">
            <FileWarning className="h-4 w-4 shrink-0" style={{ color: theme.accent }} />
            <h2 className="text-sm font-bold">近 7 天异常日志下钻明细</h2>
            <span className="font-mono text-xs tabular-nums" style={{ color: theme.textMuted }}>
              · {filteredAnomalies.length} 条记录
            </span>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            {/* 日期下拉筛选 */}
            <select
              value={selectedDay}
              onChange={(e) => setSelectedDay(e.target.value)}
              aria-label="按日期筛选异常日志"
              className="cursor-pointer rounded-lg border px-2.5 py-1.5 text-xs font-mono outline-none"
              style={{
                borderColor: theme.border,
                backgroundColor: theme.canvas,
                color: theme.textPrimary,
              }}
            >
              <option value="all">全部 7 天</option>
              {report.dailyMetrics.map((d) => (
                <option key={d.date} value={d.date}>
                  {d.date} ({d.warnCount + d.errorCount} 条告警/错误)
                </option>
              ))}
            </select>

            {/* 分类分段筛选 */}
            <div
              className="flex flex-wrap items-center gap-1 rounded-lg border p-1"
              style={{ borderColor: theme.border, backgroundColor: theme.canvas }}
            >
              {EXCEPTION_FILTERS.map((f) => {
                const active = selectedKind === f.id;
                return (
                  <button
                    key={f.id}
                    type="button"
                    onClick={() => setSelectedKind(f.id)}
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

            {/* 关键字搜索 */}
            <div
              className="flex items-center gap-1.5 rounded-lg border px-2.5 py-1.5"
              style={{ borderColor: theme.border, backgroundColor: theme.canvas }}
            >
              <Search className="h-3.5 w-3.5 shrink-0" style={{ color: theme.textMuted }} />
              <input
                type="text"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                placeholder="搜索音轨路径或错误信息..."
                className="w-44 bg-transparent text-xs outline-none placeholder:opacity-70"
                style={{ color: theme.textPrimary }}
              />
            </div>
          </div>
        </div>

        {filteredAnomalies.length === 0 ? (
          <div className="flex flex-col items-center justify-center gap-2 px-6 py-12 text-center">
            <CheckCircle2 className="h-7 w-7" style={{ color: '#16A34A' }} />
            <div className="text-sm font-semibold">当前筛选条件下无异常日志记录</div>
            <p className="max-w-md text-xs" style={{ color: theme.textMuted }}>
              {selectedDay !== 'all' || selectedKind !== 'all' || searchQuery.trim()
                ? '可以清除当前筛选条件查看近 7 天其他日期或分类的异常记录。'
                : '所解析的归档日志在近 7 天内未记录任何 WARN / ERROR 级音轨或报时异常。'}
            </p>
            {(selectedDay !== 'all' || selectedKind !== 'all' || searchQuery.trim() !== '') && (
              <button
                type="button"
                onClick={() => {
                  setSelectedDay('all');
                  setSelectedKind('all');
                  setSearchQuery('');
                }}
                className="mt-1 cursor-pointer whitespace-nowrap rounded-lg border px-3.5 py-1.5 text-xs font-semibold"
                style={{
                  borderColor: theme.border,
                  backgroundColor: theme.canvas,
                  color: theme.textPrimary,
                }}
              >
                清除全部筛选
              </button>
            )}
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full border-collapse text-left text-xs">
              <thead>
                <tr
                  className="border-b font-medium"
                  style={{ borderColor: theme.border, color: theme.textMuted }}
                >
                  <th className="whitespace-nowrap px-5 py-2.5">时间戳</th>
                  <th className="whitespace-nowrap px-4 py-2.5">归档文件</th>
                  <th className="whitespace-nowrap px-4 py-2.5">级别 / 模块</th>
                  <th className="whitespace-nowrap px-4 py-2.5">异常分类</th>
                  <th className="px-5 py-2.5">原始日志摘要</th>
                </tr>
              </thead>
              <tbody>
                {filteredAnomalies.map((row) => {
                  const meta = row.exceptionKind
                    ? EXCEPTION_KIND_META[row.exceptionKind]
                    : EXCEPTION_KIND_META.other_warning;
                  const levelColor = row.level === 'ERROR' ? '#DC2626' : '#D97706';

                  return (
                    <tr
                      key={row.id}
                      className="border-b last:border-b-0 transition-colors"
                      style={{ borderColor: theme.border }}
                    >
                      <td
                        className="whitespace-nowrap px-5 py-2.5 font-mono tabular-nums"
                        style={{ color: theme.textSecondary }}
                      >
                        {row.timestamp}
                      </td>
                      <td
                        className="whitespace-nowrap px-4 py-2.5 font-mono"
                        style={{ color: theme.textMuted }}
                      >
                        {row.sourceFile}
                      </td>
                      <td className="whitespace-nowrap px-4 py-2.5 font-mono">
                        <span className="font-semibold" style={{ color: levelColor }}>
                          {row.level}
                        </span>
                        <span className="mx-1.5" style={{ color: theme.textMuted }}>
                          /
                        </span>
                        <span style={{ color: theme.textPrimary }}>{row.target}</span>
                      </td>
                      <td className="whitespace-nowrap px-4 py-2.5">
                        <span className="font-medium" style={{ color: meta.color }}>
                          {meta.shortLabel}
                        </span>
                      </td>
                      <td
                        className="px-5 py-2.5 font-mono leading-relaxed"
                        style={{ color: theme.textPrimary }}
                      >
                        {row.message}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
