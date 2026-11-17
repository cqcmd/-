export type PerfStateKind = 'idle' | 'chime' | 'meal_duck' | 'gc_reclaim';

export type HardwareProfileId = 'low_spec_dual' | 'standard_quad' | 'high_load_cafe';

export interface PerfSamplePoint {
  id: string;
  timeLabel: string; // HH:MM:SS
  fullTimestamp: string;
  state: PerfStateKind;
  stateLabel: string;
  workingSetMb: number;
  privateMb: number;
  processCpuPct: number;
  systemCpuPct: number;
  source: string;
  cpuThrottled: boolean;
  reclaimedFromMb?: number;
  note: string;
}

export interface StateBreakdownRow {
  state: PerfStateKind;
  label: string;
  sampleCount: number;
  avgWorkingSetMb: number;
  peakWorkingSetMb: number;
  avgPrivateMb: number;
  avgProcCpuPct: number;
  peakProcCpuPct: number;
  mechanismNote: string;
}

export interface SystemOverheadReport {
  profileLabel: string;
  hardwareSpec: string;
  isUploadedLog: boolean;
  samples: PerfSamplePoint[];
  idleAvgWorkingSetMb: number;
  idleAvgPrivateMb: number;
  idleAvgCpuPct: number;
  chimePeakWorkingSetMb: number;
  chimeAvgWorkingSetMb: number;
  chimePeakCpuPct: number;
  chimeAvgCpuPct: number;
  gcReclaimCount: number;
  maxReclaimedMb: number;
  cpuThrottleCount: number;
  headroomTo50MbPct: number;
  lowSpecVerdict: 'optimal' | 'guarded' | 'attention';
  lowSpecVerdictLabel: string;
  stateBreakdowns: StateBreakdownRow[];
}

export interface HardwareProfileMeta {
  id: HardwareProfileId;
  label: string;
  shortLabel: string;
  hardwareSpec: string;
  description: string;
}

export const HARDWARE_PROFILES: HardwareProfileMeta[] = [
  {
    id: 'low_spec_dual',
    label: '低配双核 4GB 终端（SAPI5 离线）',
    shortLabel: '低配双核终端',
    hardwareSpec: 'Intel Celeron G4900 · 2C2T · 4GB RAM · Win10 LTSC',
    description: '模拟老旧网吧客户端与收银机：开启半点 SAPI5 报时、音频淡入淡出与高负载 50 MiB 自动裁剪。',
  },
  {
    id: 'standard_quad',
    label: '主流四核 8GB 无盘（Edge TTS 在线）',
    shortLabel: '主流四核无盘',
    hardwareSpec: 'Intel Core i3-10100 · 4C8T · 8GB RAM · Win11 23H2',
    description: '模拟常规电竞桌面环境：开启节假日提示音 + Microsoft Edge TTS 在线语音 + WASAPI 餐饮压低背景音乐。',
  },
  {
    id: 'high_load_cafe',
    label: '大型网游高负载值守（整机 CPU ≥ 85%）',
    shortLabel: '高负载游戏值守',
    hardwareSpec: 'AMD Ryzen 5 3600 · 整机游戏占用 86%–94% · 无盘环境',
    description: '模拟客户端运行大型游戏期间整机 CPU 持续高于 85%，验证 perf_guard 自动让步后台任务的表现。',
  },
];

export const STATE_META: Record<
  PerfStateKind,
  { label: string; color: string; mechanismNote: string }
> = {
  idle: {
    label: '空闲托盘驻留',
    color: '#64748B',
    mechanismNote: '250ms 轻量槽位扫描，隐藏主窗期间跳过 UI 字符串分配',
  },
  chime: {
    label: '半/整点报时播报',
    color: '#2563EB',
    mechanismNote: 'rodio 音频解码 + FadeEnvelopeSource 淡入淡出 + TTS 语音串联',
  },
  meal_duck: {
    label: '餐饮提醒 + WASAPI 压低',
    color: '#D97706',
    mechanismNote: 'BackgroundAudioDuckGuard 枚举音频会话并平滑压低背景音',
  },
  gc_reclaim: {
    label: '超 50 MiB 强制回收',
    color: '#DC2626',
    mechanismNote: 'trim_idle + flush_logs + HeapCompact + EmptyWorkingSet',
  },
};

// 匹配 main.rs perf_guard 与 scheduler 的真实日志格式：
// [2026-11-16 14:30:30.120] [DEBUG] [pid:4120] [perf-monitor-worker] [perf] sample (pdh+psapi): ws=18.42MB private=14.80MB proc_cpu=0.2% sys_cpu=11.4%
// [2026-11-16 14:31:00.130] [INFO ] [pid:4120] [perf-monitor-worker] [perf] memory exceeded >50MB (pdh+psapi), forced reclaim: ws 52.10MB -> 19.35MB (private=46.80MB, heap_largest_free=..., proc_cpu=1.8%, sys_cpu=24.0%)
// [2026-11-16 14:31:30.140] [WARN ] [pid:4120] [perf-monitor-worker] [perf] high CPU detected (pdh+psapi): proc_cpu=1.2% sys_cpu=89.5% (deferring non-critical background work)
const LOG_LINE_REGEX =
  /^\[(\d{4}-\d{2}-\d{2})\s+(\d{2}:\d{2}:\d{2})(?:\.\d{3})?\]\s+\[(DEBUG|INFO|WARN|ERROR)\s*\]\s+\[pid:\d+\]\s+(?:\[([^\]]+)\]\s+)?\[([^\]]+)\]\s+(.*)$/;

function toEpochSeconds(date: string, time: string): number {
  const dt = new Date(`${date}T${time}`);
  return Number.isNaN(dt.getTime()) ? 0 : Math.floor(dt.getTime() / 1000);
}

function round2(n: number): number {
  return Number(n.toFixed(2));
}

function summarizeSamples(
  samples: PerfSamplePoint[],
  meta: {
    profileLabel: string;
    hardwareSpec: string;
    isUploadedLog: boolean;
  },
): SystemOverheadReport {
  const idleSamples = samples.filter((s) => s.state === 'idle');
  const activeSamples = samples.filter((s) => s.state !== 'idle');

  const avg = (arr: PerfSamplePoint[], getter: (s: PerfSamplePoint) => number): number =>
    arr.length > 0 ? round2(arr.reduce((sum, s) => sum + getter(s), 0) / arr.length) : 0;

  const peak = (arr: PerfSamplePoint[], getter: (s: PerfSamplePoint) => number): number =>
    arr.length > 0 ? round2(Math.max(...arr.map(getter))) : 0;

  const idleAvgWorkingSetMb = avg(idleSamples, (s) => s.workingSetMb);
  const idleAvgPrivateMb = avg(idleSamples, (s) => s.privateMb);
  const idleAvgCpuPct = avg(idleSamples, (s) => s.processCpuPct);

  const chimeSource = activeSamples.length > 0 ? activeSamples : samples;
  const chimePeakWorkingSetMb = peak(chimeSource, (s) => s.workingSetMb);
  const chimeAvgWorkingSetMb = avg(chimeSource, (s) => s.workingSetMb);
  const chimePeakCpuPct = peak(chimeSource, (s) => s.processCpuPct);
  const chimeAvgCpuPct = avg(chimeSource, (s) => s.processCpuPct);

  const gcSamples = samples.filter((s) => s.state === 'gc_reclaim');
  const gcReclaimCount = gcSamples.length;
  const maxReclaimedMb =
    gcSamples.length > 0
      ? round2(
          Math.max(
            ...gcSamples.map((s) => Math.max(0, (s.reclaimedFromMb ?? s.workingSetMb) - s.workingSetMb)),
          ),
        )
      : 0;

  const cpuThrottleCount = samples.filter((s) => s.cpuThrottled).length;

  const overallPeakWs = peak(samples, (s) => s.workingSetMb);
  const headroomTo50MbPct = round2(Math.max(0, ((50 - overallPeakWs) / 50) * 100));

  let lowSpecVerdict: 'optimal' | 'guarded' | 'attention' = 'optimal';
  let lowSpecVerdictLabel = '低配适配极佳';
  if (chimePeakCpuPct >= 15 || overallPeakWs > 50) {
    lowSpecVerdict = gcReclaimCount > 0 ? 'guarded' : 'attention';
    lowSpecVerdictLabel = gcReclaimCount > 0 ? '已触发 50 MiB 自动回收稳态' : '峰值偏高需检查外部音频';
  } else if (cpuThrottleCount > 0) {
    lowSpecVerdict = 'guarded';
    lowSpecVerdictLabel = '高负载整机自动让步';
  }

  const orderedStates: PerfStateKind[] = ['idle', 'chime', 'meal_duck', 'gc_reclaim'];
  const stateBreakdowns: StateBreakdownRow[] = orderedStates
    .map((state) => {
      const subset = samples.filter((s) => s.state === state);
      if (subset.length === 0) return null;
      return {
        state,
        label: STATE_META[state].label,
        sampleCount: subset.length,
        avgWorkingSetMb: avg(subset, (s) => s.workingSetMb),
        peakWorkingSetMb: peak(subset, (s) => s.workingSetMb),
        avgPrivateMb: avg(subset, (s) => s.privateMb),
        avgProcCpuPct: avg(subset, (s) => s.processCpuPct),
        peakProcCpuPct: peak(subset, (s) => s.processCpuPct),
        mechanismNote: STATE_META[state].mechanismNote,
      };
    })
    .filter((row): row is StateBreakdownRow => row !== null);

  return {
    profileLabel: meta.profileLabel,
    hardwareSpec: meta.hardwareSpec,
    isUploadedLog: meta.isUploadedLog,
    samples,
    idleAvgWorkingSetMb,
    idleAvgPrivateMb,
    idleAvgCpuPct,
    chimePeakWorkingSetMb,
    chimeAvgWorkingSetMb,
    chimePeakCpuPct,
    chimeAvgCpuPct,
    gcReclaimCount,
    maxReclaimedMb,
    cpuThrottleCount,
    headroomTo50MbPct,
    lowSpecVerdict,
    lowSpecVerdictLabel,
    stateBreakdowns,
  };
}

/**
 * 解析包含 `HALF_HOUR_CHIME_DEBUG=1` 输出的真实 `[perf]` 与 `[scheduler]` 日志文件
 */
export function parsePerfDebugLogs(
  files: Array<{ fileName: string; content: string }>,
): SystemOverheadReport | null {
  const triggerEvents: Array<{ epoch: number; kind: 'chime' | 'meal' }> = [];
  const rawSamples: PerfSamplePoint[] = [];

  files.forEach((file, fileIdx) => {
    const lines = file.content.split(/\r?\n/);
    lines.forEach((rawLine, lineIdx) => {
      const line = rawLine.trim();
      if (!line) return;
      const m = LOG_LINE_REGEX.exec(line);
      if (!m) return;

      const [, date, time, , , target, message] = m;
      const epoch = toEpochSeconds(date, time);

      if (target === 'scheduler') {
        if (message.includes('trigger chime')) {
          triggerEvents.push({ epoch, kind: 'chime' });
        } else if (message.includes('trigger meal')) {
          triggerEvents.push({ epoch, kind: 'meal' });
        }
        return;
      }

      if (target !== 'perf') return;

      // 1) sample (pdh+psapi): ws=18.42MB private=14.80MB proc_cpu=0.2% sys_cpu=11.4%
      const sampleMatch = message.match(
        /sample\s+\(([^)]+)\):\s+ws=([\d.]+)MB\s+private=([\d.]+)MB\s+proc_cpu=([\d.]+)%\s+sys_cpu=([\d.]+)%/i,
      );
      if (sampleMatch) {
        const [, source, wsStr, privStr, procStr, sysStr] = sampleMatch;
        const sysCpu = Number(sysStr);
        const procCpu = Number(procStr);
        rawSamples.push({
          id: `${fileIdx}-${lineIdx}`,
          timeLabel: time,
          fullTimestamp: `${date} ${time}`,
          state: 'idle',
          stateLabel: STATE_META.idle.label,
          workingSetMb: Number(wsStr),
          privateMb: Number(privStr),
          processCpuPct: procCpu,
          systemCpuPct: sysCpu,
          source,
          cpuThrottled: procCpu >= 15 || sysCpu >= 85,
          note: message,
        });
        return;
      }

      // 2) memory exceeded >50MB (pdh+psapi), forced reclaim: ws 52.10MB -> 19.35MB (private=46.80MB, ..., proc_cpu=1.8%, sys_cpu=24.0%)
      const gcMatch = message.match(
        /forced reclaim:\s+ws\s+([\d.]+)MB\s*->\s*([\d.]+)MB\s+\(private=([\d.]+)MB.*?proc_cpu=([\d.]+)%,\s*sys_cpu=([\d.]+)%/i,
      );
      if (gcMatch) {
        const [, beforeStr, afterStr, privStr, procStr, sysStr] = gcMatch;
        const sourceMatch = message.match(/\(([^)]+)\)/);
        const sysCpu = Number(sysStr);
        const procCpu = Number(procStr);
        rawSamples.push({
          id: `${fileIdx}-${lineIdx}`,
          timeLabel: time,
          fullTimestamp: `${date} ${time}`,
          state: 'gc_reclaim',
          stateLabel: STATE_META.gc_reclaim.label,
          workingSetMb: Number(afterStr),
          privateMb: Number(privStr),
          processCpuPct: procCpu,
          systemCpuPct: sysCpu,
          source: sourceMatch?.[1] ?? 'pdh+psapi',
          cpuThrottled: procCpu >= 15 || sysCpu >= 85,
          reclaimedFromMb: Number(beforeStr),
          note: message,
        });
        return;
      }

      // 3) high CPU detected (pdh+psapi): proc_cpu=1.2% sys_cpu=89.5%
      const highCpuMatch = message.match(
        /high CPU detected\s+\(([^)]+)\):\s+proc_cpu=([\d.]+)%\s+sys_cpu=([\d.]+)%/i,
      );
      if (highCpuMatch) {
        const [, source, procStr, sysStr] = highCpuMatch;
        const prevWs = rawSamples[rawSamples.length - 1]?.workingSetMb ?? 19.5;
        const prevPriv = rawSamples[rawSamples.length - 1]?.privateMb ?? 15.2;
        rawSamples.push({
          id: `${fileIdx}-${lineIdx}`,
          timeLabel: time,
          fullTimestamp: `${date} ${time}`,
          state: 'idle',
          stateLabel: '整机高负载触发让步',
          workingSetMb: prevWs,
          privateMb: prevPriv,
          processCpuPct: Number(procStr),
          systemCpuPct: Number(sysStr),
          source,
          cpuThrottled: true,
          note: message,
        });
      }
    });
  });

  if (rawSamples.length === 0) {
    return null;
  }

  // 根据附近 [scheduler] trigger chime / meal 时间窗口自动标注采样点状态
  const annotated = rawSamples
    .map((sample) => {
      if (sample.state === 'gc_reclaim') return sample;
      const [datePart, timePart] = sample.fullTimestamp.split(' ');
      const sampleEpoch = toEpochSeconds(datePart, timePart);
      const nearby = triggerEvents.find(
        (ev) => sampleEpoch >= ev.epoch - 15 && sampleEpoch <= ev.epoch + 75,
      );
      if (!nearby) return sample;
      const nextState: PerfStateKind = nearby.kind === 'meal' ? 'meal_duck' : 'chime';
      return {
        ...sample,
        state: nextState,
        stateLabel: STATE_META[nextState].label,
      };
    })
    .sort((a, b) => a.fullTimestamp.localeCompare(b.fullTimestamp));

  return summarizeSamples(annotated, {
    profileLabel: `本地调试日志实测（${files.length} 个文件，${annotated.length} 个采样点）`,
    hardwareSpec: `数据来源：${annotated[0]?.source ?? 'pdh+psapi'} · 实机采集`,
    isUploadedLog: true,
  });
}

/**
 * 构建贴合 perf_guard 30s 采样周期的 30 分钟窗口（11:55 ~ 12:35，含 12:00 整点+餐饮压低、12:30 半点报时）
 */
export function buildProfileOverheadReport(profileId: HardwareProfileId): SystemOverheadReport {
  const meta = HARDWARE_PROFILES.find((p) => p.id === profileId) ?? HARDWARE_PROFILES[0];

  // 28 个采样点（覆盖 11:56 ~ 12:34，间隔约 60~90 秒展示关键节点与 30s 采样峰值）
  const timeline: Array<{
    time: string;
    state: PerfStateKind;
    ws: number;
    priv: number;
    procCpu: number;
    sysCpu: number;
    reclaimFrom?: number;
    note: string;
  }> =
    profileId === 'low_spec_dual'
      ? [
          { time: '11:56:00', state: 'idle', ws: 16.4, priv: 13.1, procCpu: 0.15, sysCpu: 18.2, note: '托盘后台静默待机，主窗隐藏' },
          { time: '11:57:30', state: 'idle', ws: 16.5, priv: 13.1, procCpu: 0.18, sysCpu: 19.5, note: '250ms 调度器空转检测' },
          { time: '11:59:00', state: 'idle', ws: 16.6, priv: 13.2, procCpu: 0.16, sysCpu: 21.0, note: '等待 12:00 整点窗口' },
          { time: '12:00:00', state: 'meal_duck', ws: 31.8, priv: 26.4, procCpu: 4.2, sysCpu: 28.4, note: '整点报时 + 餐饮提醒触发 WASAPI 背景音量压低' },
          { time: '12:00:30', state: 'meal_duck', ws: 35.4, priv: 29.8, procCpu: 4.6, sysCpu: 31.2, note: 'SAPI5 语音播报真实时间 + 恢复背景音乐' },
          { time: '12:01:30', state: 'idle', ws: 24.2, priv: 20.1, procCpu: 0.35, sysCpu: 20.8, note: '播报完毕，音频缓冲等待 trim_idle 清理' },
          { time: '12:03:00', state: 'idle', ws: 18.1, priv: 14.6, procCpu: 0.18, sysCpu: 17.6, note: 'AudioCommand::TrimIdle 释放空闲 Sink' },
          { time: '12:05:00', state: 'idle', ws: 17.3, priv: 13.8, procCpu: 0.14, sysCpu: 16.9, note: '稳定托盘常驻' },
          { time: '12:08:00', state: 'idle', ws: 17.2, priv: 13.8, procCpu: 0.15, sysCpu: 18.0, note: 'PDH 定时巡检正常' },
          { time: '12:11:00', state: 'idle', ws: 17.4, priv: 13.9, procCpu: 0.17, sysCpu: 22.1, note: '按天归档日志无新写入' },
          { time: '12:14:00', state: 'chime', ws: 46.2, priv: 41.5, procCpu: 3.4, sysCpu: 26.5, note: '试听本地高码率 FLAC 节庆音轨触发临时解码缓冲' },
          { time: '12:14:30', state: 'gc_reclaim', ws: 18.9, priv: 15.1, procCpu: 1.9, sysCpu: 25.0, reclaimFrom: 52.4, note: '工作集突破 50 MiB (52.4MB)，触发 HeapCompact + EmptyWorkingSet 强制回收' },
          { time: '12:16:30', state: 'idle', ws: 17.6, priv: 14.0, procCpu: 0.16, sysCpu: 19.2, note: 'GC 后保持低占用稳态' },
          { time: '12:20:00', state: 'idle', ws: 17.4, priv: 13.9, procCpu: 0.15, sysCpu: 17.4, note: '低配双核稳定空闲' },
          { time: '12:24:00', state: 'idle', ws: 17.5, priv: 13.9, procCpu: 0.16, sysCpu: 18.8, note: '120s GC 冷却期结束' },
          { time: '12:28:00', state: 'idle', ws: 17.5, priv: 14.0, procCpu: 0.18, sysCpu: 20.1, note: '等待 12:30 半点报时' },
          { time: '12:30:00', state: 'chime', ws: 29.6, priv: 24.1, procCpu: 3.8, sysCpu: 27.6, note: '半点报时：chime.wav 淡入淡出 + SAPI5 中文报时' },
          { time: '12:30:30', state: 'chime', ws: 27.2, priv: 22.3, procCpu: 2.4, sysCpu: 24.3, note: '语音收尾平滑淡出' },
          { time: '12:32:00', state: 'idle', ws: 18.2, priv: 14.7, procCpu: 0.21, sysCpu: 18.9, note: '恢复空闲常驻状态' },
          { time: '12:34:00', state: 'idle', ws: 17.4, priv: 13.9, procCpu: 0.15, sysCpu: 17.5, note: '平稳运行中' },
        ]
      : profileId === 'standard_quad'
        ? [
            { time: '11:56:00', state: 'idle', ws: 19.2, priv: 15.0, procCpu: 0.1, sysCpu: 12.4, note: '四核无盘托盘常驻' },
            { time: '11:57:30', state: 'idle', ws: 19.3, priv: 15.0, procCpu: 0.12, sysCpu: 11.8, note: '空闲检测零分配' },
            { time: '11:59:00', state: 'idle', ws: 19.3, priv: 15.1, procCpu: 0.11, sysCpu: 13.2, note: 'Edge TTS LRU 缓存就绪' },
            { time: '12:00:00', state: 'meal_duck', ws: 34.6, priv: 28.2, procCpu: 2.3, sysCpu: 19.6, note: '整点 + 餐饮提醒 + WASAPI 背景音乐压低至 22%' },
            { time: '12:00:30', state: 'meal_duck', ws: 38.2, priv: 31.4, procCpu: 2.6, sysCpu: 21.0, note: 'Edge TTS WebSocket 合成晓晓音色 MP3 流并缓存' },
            { time: '12:01:30', state: 'idle', ws: 25.8, priv: 21.0, procCpu: 0.24, sysCpu: 14.1, note: '背景音乐平滑恢复，清理临时音频 Sink' },
            { time: '12:03:00', state: 'idle', ws: 21.4, priv: 16.8, procCpu: 0.12, sysCpu: 12.0, note: '保留 Edge TTS 热门短语热缓存 (LRU <= 4MB)' },
            { time: '12:06:00', state: 'idle', ws: 21.2, priv: 16.6, procCpu: 0.11, sysCpu: 11.5, note: '平稳待机' },
            { time: '12:10:00', state: 'idle', ws: 21.1, priv: 16.6, procCpu: 0.1, sysCpu: 12.8, note: 'PDH + PSAPI 双路采样正常' },
            { time: '12:14:00', state: 'idle', ws: 21.3, priv: 16.7, procCpu: 0.14, sysCpu: 15.4, note: '后台检查更新定时器待命' },
            { time: '12:18:00', state: 'idle', ws: 21.2, priv: 16.6, procCpu: 0.11, sysCpu: 13.0, note: '平稳待机' },
            { time: '12:22:00', state: 'idle', ws: 21.0, priv: 16.5, procCpu: 0.1, sysCpu: 12.1, note: '平稳待机' },
            { time: '12:26:00', state: 'idle', ws: 21.1, priv: 16.5, procCpu: 0.11, sysCpu: 12.6, note: '平稳待机' },
            { time: '12:30:00', state: 'chime', ws: 31.4, priv: 25.6, procCpu: 1.8, sysCpu: 17.5, note: '12:30 半点报时命中 Edge TTS LRU 缓存（零网络耗时）' },
            { time: '12:30:30', state: 'chime', ws: 29.8, priv: 24.0, procCpu: 1.2, sysCpu: 15.2, note: 'MP3 解码淡入淡出播报完成' },
            { time: '12:32:00', state: 'idle', ws: 21.5, priv: 16.8, procCpu: 0.12, sysCpu: 12.3, note: '回归稳态' },
            { time: '12:34:00', state: 'idle', ws: 21.2, priv: 16.6, procCpu: 0.1, sysCpu: 11.9, note: '远低于 50 MiB 阈值' },
          ]
        : [
            { time: '11:56:00', state: 'idle', ws: 18.6, priv: 14.5, procCpu: 0.16, sysCpu: 72.0, note: '客户端启动大型 3D 电竞游戏' },
            { time: '11:57:30', state: 'idle', ws: 18.5, priv: 14.4, procCpu: 0.18, sysCpu: 87.4, note: '整机 CPU >= 85%，perf_guard 置 cpu_throttled=true 让步非关键网络检查' },
            { time: '11:59:00', state: 'idle', ws: 18.4, priv: 14.4, procCpu: 0.15, sysCpu: 91.2, note: '推迟后台更新检查与冗余缓存分配，保障游戏帧率' },
            { time: '12:00:00', state: 'meal_duck', ws: 32.1, priv: 26.8, procCpu: 2.4, sysCpu: 92.6, note: '高负载期间仍准时触发 12:00 餐饮提醒（误差 <250ms）' },
            { time: '12:00:30', state: 'meal_duck', ws: 33.4, priv: 27.6, procCpu: 2.1, sysCpu: 89.8, note: '播报完毕立即释放空闲音频缓冲' },
            { time: '12:02:00', state: 'idle', ws: 19.1, priv: 15.0, procCpu: 0.2, sysCpu: 86.5, note: '持续高负载，保持后台任务让步' },
            { time: '12:06:00', state: 'idle', ws: 18.8, priv: 14.8, procCpu: 0.16, sysCpu: 88.0, note: '本进程占用始终 <0.25%' },
            { time: '12:12:00', state: 'idle', ws: 18.7, priv: 14.7, procCpu: 0.14, sysCpu: 76.4, note: '游戏切局结束，整机 CPU 回落至 <85%，恢复常规巡检' },
            { time: '12:18:00', state: 'idle', ws: 18.9, priv: 14.8, procCpu: 0.15, sysCpu: 68.5, note: '平稳常驻' },
            { time: '12:24:00', state: 'idle', ws: 19.0, priv: 14.9, procCpu: 0.16, sysCpu: 86.2, note: '再次进入团战高负载，自动延后非核心任务' },
            { time: '12:30:00', state: 'chime', ws: 28.5, priv: 23.2, procCpu: 1.9, sysCpu: 90.4, note: '半点报时离线 SAPI5 稳定播报，无卡顿叠音' },
            { time: '12:30:30', state: 'chime', ws: 26.2, priv: 21.0, procCpu: 1.1, sysCpu: 88.1, note: '报时结束' },
            { time: '12:32:00', state: 'idle', ws: 18.9, priv: 14.8, procCpu: 0.17, sysCpu: 85.4, note: '恢复极低占空比' },
            { time: '12:34:00', state: 'idle', ws: 18.7, priv: 14.6, procCpu: 0.15, sysCpu: 79.0, note: '全程未触发 50 MiB 告警' },
          ];

  const today = new Date().toISOString().slice(0, 10);
  const samples: PerfSamplePoint[] = timeline.map((item, idx) => ({
    id: `${profileId}-${idx}`,
    timeLabel: item.time,
    fullTimestamp: `${today} ${item.time}`,
    state: item.state,
    stateLabel: STATE_META[item.state].label,
    workingSetMb: item.ws,
    privateMb: item.priv,
    processCpuPct: item.procCpu,
    systemCpuPct: item.sysCpu,
    source: 'pdh+psapi',
    cpuThrottled: item.procCpu >= 15 || item.sysCpu >= 85,
    reclaimedFromMb: item.reclaimFrom,
    note: item.note,
  }));

  return summarizeSamples(samples, {
    profileLabel: meta.label,
    hardwareSpec: meta.hardwareSpec,
    isUploadedLog: false,
  });
}
