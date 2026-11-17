export interface DriveFileItem {
  id: string;
  name: string;
  mimeType: string;
  size?: string;
  modifiedTime?: string;
  webViewLink?: string;
  trashed?: boolean;
}

export class DriveAuthExpiredError extends Error {
  constructor(message = 'Google Drive 授权已过期，请重新登录。') {
    super(message);
    this.name = 'DriveAuthExpiredError';
  }
}

const DRIVE_FILES_API = 'https://www.googleapis.com/drive/v3/files';
const DRIVE_UPLOAD_API = 'https://www.googleapis.com/upload/drive/v3/files';

async function parseDriveError(res: Response): Promise<never> {
  if (res.status === 401) {
    throw new DriveAuthExpiredError();
  }
  let detail = `HTTP ${res.status}`;
  try {
    const body = await res.json();
    if (body?.error?.message) {
      detail = body.error.message;
    }
  } catch {
    // ignore JSON parse errors
  }
  throw new Error(detail);
}

export async function listDriveFiles(
  accessToken: string,
  options?: { queryText?: string; filterKind?: 'all' | 'audio' | 'config' }
): Promise<DriveFileItem[]> {
  const qParts: string[] = ['trashed = false'];
  const text = options?.queryText?.trim();
  if (text) {
    const escaped = text.replace(/\\/g, '\\\\').replace(/'/g, "\\'");
    qParts.push(`name contains '${escaped}'`);
  }
  if (options?.filterKind === 'audio') {
    qParts.push(
      "(mimeType contains 'audio/' or name contains '.wav' or name contains '.mp3' or name contains '.ogg' or name contains '.flac')"
    );
  } else if (options?.filterKind === 'config') {
    qParts.push(
      "(mimeType = 'application/json' or name contains '.json' or name contains '.md' or name contains '.txt')"
    );
  }

  const params = new URLSearchParams({
    pageSize: '50',
    orderBy: 'modifiedTime desc',
    q: qParts.join(' and '),
    fields: 'files(id,name,mimeType,size,modifiedTime,webViewLink,trashed)',
  });

  const res = await fetch(`${DRIVE_FILES_API}?${params.toString()}`, {
    headers: {
      Authorization: `Bearer ${accessToken}`,
    },
  });

  if (!res.ok) {
    await parseDriveError(res);
  }
  const data = await res.json();
  return (data.files as DriveFileItem[]) ?? [];
}

export async function uploadDriveFile(
  accessToken: string,
  fileName: string,
  mimeType: string,
  content: Blob | string
): Promise<DriveFileItem> {
  const metadata = {
    name: fileName,
    mimeType,
  };

  const form = new FormData();
  form.append('metadata', new Blob([JSON.stringify(metadata)], { type: 'application/json' }));
  form.append(
    'file',
    typeof content === 'string' ? new Blob([content], { type: mimeType }) : content
  );

  const res = await fetch(
    `${DRIVE_UPLOAD_API}?uploadType=multipart&fields=id,name,mimeType,size,modifiedTime,webViewLink`,
    {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${accessToken}`,
      },
      body: form,
    }
  );

  if (!res.ok) {
    await parseDriveError(res);
  }
  return (await res.json()) as DriveFileItem;
}

export async function downloadDriveFileBlob(
  accessToken: string,
  file: DriveFileItem
): Promise<Blob> {
  const isGoogleDoc = file.mimeType.startsWith('application/vnd.google-apps.');
  const url = isGoogleDoc
    ? `${DRIVE_FILES_API}/${encodeURIComponent(file.id)}/export?mimeType=application/pdf`
    : `${DRIVE_FILES_API}/${encodeURIComponent(file.id)}?alt=media`;

  const res = await fetch(url, {
    headers: {
      Authorization: `Bearer ${accessToken}`,
    },
  });

  if (!res.ok) {
    await parseDriveError(res);
  }
  return await res.blob();
}

export async function renameDriveFile(
  accessToken: string,
  fileId: string,
  newName: string
): Promise<DriveFileItem> {
  const res = await fetch(
    `${DRIVE_FILES_API}/${encodeURIComponent(fileId)}?fields=id,name,mimeType,size,modifiedTime,webViewLink`,
    {
      method: 'PATCH',
      headers: {
        Authorization: `Bearer ${accessToken}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({ name: newName }),
    }
  );

  if (!res.ok) {
    await parseDriveError(res);
  }
  return (await res.json()) as DriveFileItem;
}

export async function deleteDriveFile(accessToken: string, fileId: string): Promise<void> {
  const res = await fetch(`${DRIVE_FILES_API}/${encodeURIComponent(fileId)}`, {
    method: 'DELETE',
    headers: {
      Authorization: `Bearer ${accessToken}`,
    },
  });

  if (!res.ok) {
    await parseDriveError(res);
  }
}
