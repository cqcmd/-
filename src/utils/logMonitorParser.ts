export type LogLevel = 'DEBUG' | 'INFO' | 'WARN' | 'ERROR';

export type AudioExceptionKind =
  | 'file_missing'
  | 'decode_error'
  | 'tts_fallback'
  | 'device_stream'
  | 'other_warning';

export interface ParsedLogEntry {
  id: string;
  date: string; // YYYY-MM-DD
  time: string; // HH:MM:SS.mmm
  timestamp: string;
  level: LogLevel;
  pid: string;
  thread: string;
  target: string;
  message: string;
  sourceFile: string;
  isChimeTrigger: boolean;
  isMealTrigger: boolean;
  isChimeFailure: boolean;
  exceptionKind: AudioExceptionKind | null;
}

export interface DailyMonitorMetric {
  date: string; // YYYY-MM-DD
  shortDate: string; // MM-DD
  totalChimes: number;
  failedChimes: number;
  failureRatePct: number;
  fileMissingCount: number;
  decodeErrorCount: number;
  ttsFallbackCount: number;
  deviceStreamCount: number;
  totalAudioExceptions: number;
  warnCount: number;
  errorCount: number;
  splitRolloverCount: number;
  archiveFiles: string[];
}

export interface ErrorMonitorReport {
  sourceLabel: string;
  isSampleData: boolean;
  dateRangeLabel: string;
  dailyMetrics: DailyMonitorMetric[];
  anomalies: ParsedLogEntry[];
  totalLinesParsed: number;
  unparsedLines: number;
  totalFilesAnalyzed: number;
  totalChimes7d: number;
  totalFailedChimes7d: number;
  overallFailureRatePct: number;
  totalAudioExceptions7d: number;
  peakFailureDay: DailyMonitorMetric | null;
  topExceptionCategory: {
    kind: AudioExceptionKind;
    label: string;
    count: number;
  } | null;
  totalSplitRollovers: number;
}

export interface RawLogFileInput {
  fileName: string;
  content: string;
}

export const EXCEPTION_KIND_META: Record<
  AudioExceptionKind,
  { label: string; shortLabel: string; color: string }
> = {
  file_missing: {
    label: '音频文件缺失 / 权限拒绝',
    shortLabel: '文件缺失',
    color: '#DC2626',
  },
  decode_error: {
    label: '音轨格式解码失败',
    shortLabel: '解码失败',
    color: '#EA580C',
  },
  tts_fallback: {
    label: 'Edge TTS 回退 / 语音合成异常',
    shortLabel: 'TTS 回退',
    color: '#D97706',
  },
  device_stream: {
    label: '音频输出流 / WASAPI 异常',
    shortLabel: '设备与流',
    color: '#7C3AED',
  },
  other_warning: {
    label: '其他告警与同步异常',
    shortLabel: '其他告警',
    color: '#64748B',
  },
};

// 匹配 win_sys.rs RotatingLogger::log_at 与 rollover_to_date 格式：
// [2026-11-16 14:30:00.125] [WARN ] [pid:4820] [audio-worker] [audio] open custom meal audio "..." failed: ...
// 或无线程段：
// [2026-11-16 00:00:00.012] [INFO ] [pid:4820] [logger] --- daily log archived/started ... ---
const LOG_LINE_REGEX =
  /^\[(\d{4}-\d{2}-\d{2})\s+(\d{2}:\d{2}:\d{2}(?:\.\d{3})?)\]\s+\[(DEBUG|INFO|WARN|ERROR)\s*\]\s+\[pid:(\d+)\]\s+(?:\[([^\]]+)\]\s+)?\[([^\]]+)\]\s+(.*)$/;

function normalizeLevel(raw: string): LogLevel {
  const trimmed = raw.trim().toUpperCase();
  if (trimmed === 'ERROR') return 'ERROR';
  if (trimmed === 'WARN') return 'WARN';
  if (trimmed === 'DEBUG') return 'DEBUG';
  return 'INFO';
}

function classifyAudioException(
  level: LogLevel,
  target: string,
  message: string,
): { exceptionKind: AudioExceptionKind | null; isChimeFailure: boolean } {
  const lowerTarget = target.toLowerCase();
  const lowerMsg = message.toLowerCase();

  if (level === 'INFO' || level === 'DEBUG') {
    return { exceptionKind: null, isChimeFailure: false };
  }

  // 1. 音频文件缺失 / 无法打开（来自 audio.rs: open custom ... audio failed）
  if (
    lowerMsg.includes('open custom') ||
    lowerMsg.includes('no such file') ||
    lowerMsg.includes('cannot find the file') ||
    lowerMsg.includes('permission denied') ||
    lowerMsg.includes('找不到指定的文件') ||
    lowerMsg.includes('找不到指定的路径')
  ) {
    return { exceptionKind: 'file_missing', isChimeFailure: true };
  }

  // 2. 音轨解码失败（来自 audio.rs: decode custom ... failed / MP3 decoder unavailable）
  if (
    lowerMsg.includes('decode custom') ||
    lowerMsg.includes('decoder unavailable') ||
    lowerMsg.includes('unrecognizedformat') ||
    lowerMsg.includes('corrupt') ||
    lowerMsg.includes('decode error') ||
    lowerMsg.includes('解码')
  ) {
    return { exceptionKind: 'decode_error', isChimeFailure: true };
  }

  // 3. Edge TTS 网络回退 / SAPI5 合成失败（来自 audio.rs / win_sys.rs target=tts）
  if (
    lowerTarget === 'tts' ||
    lowerMsg.includes('edge tts') ||
    lowerMsg.includes('sapi') ||
    lowerMsg.includes('ispvoice') ||
    lowerMsg.includes('语音引擎失败') ||
    lowerMsg.includes('语音播报失败')
  ) {
    // Edge TTS 回退至 SAPI5 属于降级告警，只有当 SAPI5 也失败或明确合成失败时计入报时失败
    const hardTtsFailure =
      level === 'ERROR' ||
      (lowerMsg.includes('sapi') && lowerMsg.includes('failed')) ||
      lowerMsg.includes('speak_text_sapi') ||
      lowerMsg.includes('语音引擎失败') ||
      lowerMsg.includes('语音播报失败');
    return {
      exceptionKind: 'tts_fallback',
      isChimeFailure: hardTtsFailure,
    };
  }

  // 4. 音频输出流 / WASAPI 设备压低或切换失败
  if (
    lowerTarget === 'audio' ||
    lowerMsg.includes('outputstream') ||
    lowerMsg.includes('wasapi') ||
    lowerMsg.includes('audio') ||
    lowerMsg.includes('sink')
  ) {
    return { exceptionKind: 'device_stream', isChimeFailure: true };
  }

  // 5. 调度器显式报时失败
  if (
    lowerTarget === 'scheduler' &&
    (level === 'ERROR' || lowerMsg.includes('chime') || lowerMsg.includes('failed'))
  ) {
    return { exceptionKind: 'device_stream', isChimeFailure: true };
  }

  // 6. 其他 WARN / ERROR（如 ntp、updater、holiday、config）
  return {
    exceptionKind: 'other_warning',
    isChimeFailure: level === 'ERROR' && lowerMsg.includes('chime'),
  };
}

function formatYmd(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

function buildSevenDayAxis(anchorDateStr: string): string[] {
  const parsed = new Date(`${anchorDateStr}T12:00:00`);
  const base = Number.isNaN(parsed.getTime()) ? new Date() : parsed;
  const days: string[] = [];
  for (let offset = 6; offset >= 0; offset -= 1) {
    const d = new Date(base);
    d.setDate(base.getDate() - offset);
    days.push(formatYmd(d));
  }
  return days;
}

export function extractDateFromArchiveName(fileName: string): string | null {
  const m = fileName.match(/(\d{4}-\d{2}-\d{2})\.log(?:\.\d+)?$/i);
  return m ? m[1] : null;
}

export function parseRotatingLogs(
  files: RawLogFileInput[],
  options?: {
    sourceLabel?: string;
    isSampleData?: boolean;
    anchorDate?: string;
  },
): ErrorMonitorReport {
  const entries: ParsedLogEntry[] = [];
  const discoveredDates = new Set<string>();
  const splitRolloverDates = new Map<string, number>();
  const archiveFilesByDate = new Map<string, Set<string>>();

  let totalLinesParsed = 0;
  let unparsedLines = 0;

  files.forEach((file, fileIdx) => {
    const fileDateHint = extractDateFromArchiveName(file.fileName);
    if (fileDateHint) {
      discoveredDates.add(fileDateHint);
      if (!archiveFilesByDate.has(fileDateHint)) {
        archiveFilesByDate.set(fileDateHint, new Set());
      }
      archiveFilesByDate.get(fileDateHint)!.add(file.fileName);
    }

    const lines = file.content.split(/\r?\n/);
    lines.forEach((rawLine, lineIdx) => {
      const line = rawLine.trim();
      if (!line) return;

      const match = LOG_LINE_REGEX.exec(line);
      if (!match) {
        unparsedLines += 1;
        return;
      }

      totalLinesParsed += 1;
      const [, date, time, rawLevel, pid, maybeThread, target, message] = match;
      const level = normalizeLevel(rawLevel);
      const thread = maybeThread ?? 'main';

      discoveredDates.add(date);
      if (!archiveFilesByDate.has(date)) {
        archiveFilesByDate.set(date, new Set());
      }
      archiveFilesByDate.get(date)!.add(file.fileName);

      if (target === 'logger' && message.includes('same-day log split')) {
        splitRolloverDates.set(date, (splitRolloverDates.get(date) ?? 0) + 1);
      }

      const isChimeTrigger = target === 'scheduler' && message.includes('trigger chime');
      const isMealTrigger = target === 'scheduler' && message.includes('trigger meal');
      const { exceptionKind, isChimeFailure } = classifyAudioException(level, target, message);

      entries.push({
        id: `${fileIdx}-${lineIdx}-${date}-${time}`,
        date,
        time,
        timestamp: `${date} ${time}`,
        level,
        pid,
        thread,
        target,
        message,
        sourceFile: file.fileName,
        isChimeTrigger,
        isMealTrigger,
        isChimeFailure,
        exceptionKind,
      });
    });
  });

  const sortedDates = Array.from(discoveredDates).sort();
  const anchorDate =
    options?.anchorDate ??
    (sortedDates.length > 0 ? sortedDates[sortedDates.length - 1] : formatYmd(new Date()));
  const sevenDayAxis = buildSevenDayAxis(anchorDate);
  const sevenDaySet = new Set(sevenDayAxis);

  const dayMap = new Map<string, DailyMonitorMetric>();
  sevenDayAxis.forEach((date) => {
    dayMap.set(date, {
      date,
      shortDate: date.slice(5),
      totalChimes: 0,
      failedChimes: 0,
      failureRatePct: 0,
      fileMissingCount: 0,
      decodeErrorCount: 0,
      ttsFallbackCount: 0,
      deviceStreamCount: 0,
      totalAudioExceptions: 0,
      warnCount: 0,
      errorCount: 0,
      splitRolloverCount: splitRolloverDates.get(date) ?? 0,
      archiveFiles: Array.from(archiveFilesByDate.get(date) ?? []),
    });
  });

  const anomalies: ParsedLogEntry[] = [];

  for (const entry of entries) {
    if (!sevenDaySet.has(entry.date)) continue;
    const metric = dayMap.get(entry.date);
    if (!metric) continue;

    if (entry.isChimeTrigger) {
      metric.totalChimes += 1;
    }
    if (entry.level === 'WARN') metric.warnCount += 1;
    if (entry.level === 'ERROR') metric.errorCount += 1;

    if (entry.isChimeFailure) {
      metric.failedChimes += 1;
    }

    if (entry.exceptionKind) {
      anomalies.push(entry);
      if (entry.exceptionKind === 'file_missing') {
        metric.fileMissingCount += 1;
        metric.totalAudioExceptions += 1;
      } else if (entry.exceptionKind === 'decode_error') {
        metric.decodeErrorCount += 1;
        metric.totalAudioExceptions += 1;
      } else if (entry.exceptionKind === 'tts_fallback') {
        metric.ttsFallbackCount += 1;
        metric.totalAudioExceptions += 1;
      } else if (entry.exceptionKind === 'device_stream') {
        metric.deviceStreamCount += 1;
        metric.totalAudioExceptions += 1;
      }
    }
  }

  const dailyMetrics = sevenDayAxis.map((date) => {
    const metric = dayMap.get(date)!;
    // 若某日仅记录了异常而未记录正常 INFO 报时触发（例如仅上传了部分过滤日志），用 failedChimes 作为分母下限
    const denominator = Math.max(metric.totalChimes, metric.failedChimes);
    metric.failureRatePct =
      denominator > 0 ? Number(((metric.failedChimes / denominator) * 100).toFixed(2)) : 0;
    return metric;
  });

  const totalChimes7d = dailyMetrics.reduce((acc, d) => acc + d.totalChimes, 0);
  const totalFailedChimes7d = dailyMetrics.reduce((acc, d) => acc + d.failedChimes, 0);
  const totalAudioExceptions7d = dailyMetrics.reduce((acc, d) => acc + d.totalAudioExceptions, 0);
  const totalSplitRollovers = dailyMetrics.reduce((acc, d) => acc + d.splitRolloverCount, 0);

  const denominator7d = Math.max(totalChimes7d, totalFailedChimes7d);
  const overallFailureRatePct =
    denominator7d > 0 ? Number(((totalFailedChimes7d / denominator7d) * 100).toFixed(2)) : 0;

  const peakFailureDay =
    dailyMetrics
      .filter((d) => d.failedChimes > 0 || d.totalAudioExceptions > 0)
      .sort((a, b) => b.failureRatePct - a.failureRatePct || b.failedChimes - a.failedChimes)[0] ??
    null;

  const categoryTotals: Record<AudioExceptionKind, number> = {
    file_missing: dailyMetrics.reduce((s, d) => s + d.fileMissingCount, 0),
    decode_error: dailyMetrics.reduce((s, d) => s + d.decodeErrorCount, 0),
    tts_fallback: dailyMetrics.reduce((s, d) => s + d.ttsFallbackCount, 0),
    device_stream: dailyMetrics.reduce((s, d) => s + d.deviceStreamCount, 0),
    other_warning: anomalies.filter((a) => a.exceptionKind === 'other_warning').length,
  };

  const audioCategoryCandidates: AudioExceptionKind[] = [
    'file_missing',
    'decode_error',
    'tts_fallback',
    'device_stream',
  ];
  const topKind = audioCategoryCandidates
    .map((kind) => ({ kind, count: categoryTotals[kind] }))
    .sort((a, b) => b.count - a.count)[0];

  anomalies.sort((a, b) => b.timestamp.localeCompare(a.timestamp));

  return {
    sourceLabel:
      options?.sourceLabel ??
      `已解析 ${files.length} 个归档日志文件（${sevenDayAxis[0]} 至 ${sevenDayAxis[6]}）`,
    isSampleData: Boolean(options?.isSampleData),
    dateRangeLabel: `${sevenDayAxis[0]} ~ ${sevenDayAxis[6]}`,
    dailyMetrics,
    anomalies,
    totalLinesParsed,
    unparsedLines,
    totalFilesAnalyzed: files.length,
    totalChimes7d,
    totalFailedChimes7d,
    overallFailureRatePct,
    totalAudioExceptions7d,
    peakFailureDay,
    topExceptionCategory:
      topKind && topKind.count > 0
        ? {
            kind: topKind.kind,
            label: EXCEPTION_KIND_META[topKind.kind].label,
            count: topKind.count,
          }
        : null,
    totalSplitRollovers,
  };
}

/**
 * 基于 win_sys.rs RotatingLogger 格式生成近 7 天基准归档日志，供离线工作台即开即查与导出验证
 */
export function buildSampleSevenDayLogs(anchorDateStr?: string): RawLogFileInput[] {
  const anchor = anchorDateStr ?? formatYmd(new Date());
  const days = buildSevenDayAxis(anchor);

  const dayProfiles: Array<{
    totalChimes: number;
    fileMissing: Array<{ hhmm: string; path: string; reason: string }>;
    decodeErrors: Array<{ hhmm: string; path: string; reason: string }>;
    ttsFallbacks: Array<{ hhmm: string; reason: string; hardFail?: boolean }>;
    deviceErrors: Array<{ hhmm: string; reason: string }>;
    splitOverflow?: boolean;
  }> = [
    // Day -6
    {
      totalChimes: 34,
      fileMissing: [],
      decodeErrors: [],
      ttsFallbacks: [{ hhmm: '14:30:01.104', reason: 'timeout after 4000ms (Edge TTS)' }],
      deviceErrors: [],
    },
    // Day -5
    {
      totalChimes: 36,
      fileMissing: [
        {
          hhmm: '12:00:00.218',
          path: 'D:\\Media\\meal_lunch.mp3',
          reason: '系统找不到指定的文件。 (os error 2)',
        },
      ],
      decodeErrors: [],
      ttsFallbacks: [],
      deviceErrors: [],
    },
    // Day -4
    {
      totalChimes: 34,
      fileMissing: [],
      decodeErrors: [
        {
          hhmm: '18:00:00.340',
          path: 'D:\\Media\\dinner_notice.ogg',
          reason: 'UnrecognizedFormat: invalid Ogg page header',
        },
      ],
      ttsFallbacks: [
        { hhmm: '19:30:01.412', reason: 'Edge TTS HTTP 503 handshake error' },
        { hhmm: '21:00:01.089', reason: 'Edge TTS TLS connection reset by peer' },
      ],
      deviceErrors: [],
    },
    // Day -3
    {
      totalChimes: 36,
      fileMissing: [],
      decodeErrors: [],
      ttsFallbacks: [],
      deviceErrors: [],
    },
    // Day -2 (网吧更换盘符 + 节庆音频异常峰值日)
    {
      totalChimes: 38,
      fileMissing: [
        {
          hhmm: '10:00:00.145',
          path: 'E:\\CafeAudio\\chime_fest.wav',
          reason: '系统找不到指定的路径。 (os error 3)',
        },
        {
          hhmm: '12:00:00.190',
          path: 'E:\\CafeAudio\\meal_combo.mp3',
          reason: '系统找不到指定的路径。 (os error 3)',
        },
      ],
      decodeErrors: [
        {
          hhmm: '15:30:00.275',
          path: 'D:\\Media\\promo_corrupt.flac',
          reason: 'StreamError: FLAC frame CRC mismatch',
        },
      ],
      ttsFallbacks: [
        {
          hhmm: '16:00:01.620',
          reason: 'Edge TTS MP3 decoder unavailable, falling back to local SAPI5',
        },
      ],
      deviceErrors: [
        {
          hhmm: '20:30:00.095',
          reason: 'WASAPI OutputStream device invalidated (headset unplugged)',
        },
      ],
      splitOverflow: true,
    },
    // Day -1
    {
      totalChimes: 36,
      fileMissing: [],
      decodeErrors: [],
      ttsFallbacks: [{ hhmm: '13:30:01.301', reason: 'Edge TTS DNS lookup timeout' }],
      deviceErrors: [],
    },
    // Day 0 (Today)
    {
      totalChimes: 28,
      fileMissing: [
        {
          hhmm: '12:00:00.112',
          path: 'D:\\Media\\weekend_meal.wav',
          reason: '拒绝访问。 (os error 5)',
        },
      ],
      decodeErrors: [],
      ttsFallbacks: [{ hhmm: '15:00:01.044', reason: 'Edge TTS proxy unreachable' }],
      deviceErrors: [],
    },
  ];

  return days.map((date, idx) => {
    const profile = dayProfiles[idx] ?? dayProfiles[0];
    const pid = 4120 + idx * 17;
    const lines: string[] = [
      `[${date} 00:00:00.015] [INFO ] [pid:${pid}] [logger] --- daily log archived/started (date=${date}, retention_days=14) ---`,
      `[${date} 08:00:01.040] [INFO ] [pid:${pid}] [main] [env] startup version=2.0.0+${date.replace(/-/g, '')} os=windows arch=x86_64 computer=CAFE-HOST-01 user=Administrator`,
    ];

    if (profile.splitOverflow) {
      lines.push(
        `[${date} 16:05:12.008] [INFO ] [pid:${pid}] [logger] --- same-day log split (max_bytes=2097152, backups=3) ---`,
      );
    }

    // 生成半整点调度触发记录（08:00 起每 30 分钟一次，直至达到 totalChimes）
    for (let i = 0; i < profile.totalChimes; i += 1) {
      const totalMins = 8 * 60 + i * 30;
      const hh = String(Math.floor(totalMins / 60) % 24).padStart(2, '0');
      const mm = String(totalMins % 60).padStart(2, '0');
      lines.push(
        `[${date} ${hh}:${mm}:00.080] [INFO ] [pid:${pid}] [main] [scheduler] trigger chime repeat=1 volume=80 sound=true holiday=None holiday_audio=None tts=Some("现在时间，${hh}点${mm === '00' ? '整' : '三十分'}")`,
      );
    }

    profile.fileMissing.forEach((item) => {
      lines.push(
        `[${date} ${item.hhmm}] [WARN ] [pid:${pid}] [audio-worker] [audio] open custom meal audio "${item.path}" failed: ${item.reason}`,
      );
    });

    profile.decodeErrors.forEach((item) => {
      lines.push(
        `[${date} ${item.hhmm}] [WARN ] [pid:${pid}] [audio-worker] [audio] decode custom meal audio "${item.path}" failed: ${item.reason}`,
      );
    });

    profile.ttsFallbacks.forEach((item) => {
      lines.push(
        `[${date} ${item.hhmm}] [WARN ] [pid:${pid}] [audio-sequence-worker] [tts] Edge TTS unavailable (${item.reason}), falling back to local SAPI5`,
      );
    });

    profile.deviceErrors.forEach((item) => {
      lines.push(
        `[${date} ${item.hhmm}] [ERROR] [pid:${pid}] [audio-worker] [audio] ${item.reason}`,
      );
    });

    return {
      fileName: `half_hour_chime_${date}.log`,
      content: `${lines.join('\r\n')}\r\n`,
    };
  });
}
