#!/usr/bin/env node

import { readFile, writeFile } from 'node:fs/promises';
import { basename, extname, resolve } from 'node:path';
import { spawn } from 'node:child_process';

const BUFFER_API_URL = 'https://api.buffer.com';
const BUFFER_THREAD_DOC_URL = 'https://developers.buffer.com/types/TwitterPostMetadataInput.html';

let bufferAccessToken = '';

function redact(value) {
  let text = String(value ?? '');
  if (bufferAccessToken) {
    text = text.split(bufferAccessToken).join('[REDACTED_BUFFER_ACCESS_TOKEN]');
  }
  return text;
}

function fatal(message, exitCode) {
  console.error(redact(message));
  process.exit(exitCode);
}

function formatLocalDate(date) {
  const yyyy = String(date.getFullYear());
  const mm = String(date.getMonth() + 1).padStart(2, '0');
  const dd = String(date.getDate()).padStart(2, '0');
  return `${yyyy}-${mm}-${dd}`;
}

function parseArgs(argv) {
  const args = {
    artifact: `resource/growth-loop/${formatLocalDate(new Date())}.md`,
    channelId: '',
    dryRun: false,
    service: 'twitter',
  };

  for (let index = 2; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--dry-run') {
      args.dryRun = true;
      continue;
    }
    if (arg === '--artifact') {
      const value = argv[index + 1];
      if (!value || value.startsWith('--')) {
        fatal('--artifact requires a path.', 1);
      }
      args.artifact = value;
      index += 1;
      continue;
    }
    if (arg === '--channel-id') {
      const value = argv[index + 1];
      if (!value || value.startsWith('--')) {
        fatal('--channel-id requires an id.', 1);
      }
      args.channelId = value;
      index += 1;
      continue;
    }
    if (arg === '--service') {
      const value = argv[index + 1];
      if (!value || value.startsWith('--')) {
        fatal('--service requires a value (twitter|threads).', 1);
      }
      if (value !== 'twitter' && value !== 'threads') {
        fatal(`--service must be 'twitter' or 'threads' (got '${value}').`, 1);
      }
      args.service = value;
      index += 1;
      continue;
    }
    fatal(`Unknown argument: ${arg}`, 1);
  }

  if (!args.channelId) {
    fatal('--channel-id is required.', 1);
  }

  return args;
}

function validateEnvironment() {
  const nodeOptions = process.env.NODE_OPTIONS ?? '';
  if (nodeOptions.includes('--inspect')) {
    fatal('Refusing to run with NODE_OPTIONS containing --inspect.', 1);
  }

  bufferAccessToken = process.env.BUFFER_ACCESS_TOKEN ?? '';
  if (!bufferAccessToken) {
    fatal('BUFFER_ACCESS_TOKEN not set. See scripts/publish-to-buffer.README.md.', 1);
  }

  const r2PublicBaseUrl = process.env.R2_PUBLIC_BASE_URL ?? '';
  if (!r2PublicBaseUrl) {
    fatal('R2_PUBLIC_BASE_URL not set. See scripts/publish-to-buffer.README.md.', 1);
  }

  return {
    r2PublicBaseUrl: r2PublicBaseUrl.replace(/\/+$/, ''),
    r2Bucket: process.env.R2_BUCKET || 'spur-growth-loop',
  };
}

function sectionBody(markdown, headingRegex, nextHeadingRegex) {
  const headingMatch = headingRegex.exec(markdown);
  if (!headingMatch) {
    return '';
  }
  const start = headingMatch.index + headingMatch[0].length;
  nextHeadingRegex.lastIndex = start;
  const nextMatch = nextHeadingRegex.exec(markdown);
  const end = nextMatch ? nextMatch.index : markdown.length;
  return markdown.slice(start, end).trim();
}

function extractDraftsXSection(markdown) {
  const startMatch = /^##\s+Drafts\s+[—–-]\s+X\s*$/m.exec(markdown);
  if (!startMatch) {
    throw new Error("Artifact is missing '## Drafts — X'.");
  }

  const start = startMatch.index + startMatch[0].length;
  const rest = markdown.slice(start);
  const nextMatch = /^##\s+/m.exec(rest);
  return rest.slice(0, nextMatch ? nextMatch.index : rest.length).trim();
}

function cleanPostBody(body) {
  return body
    .split('\n')
    .map((line) => line.replace(/^\s*>\s?/, '').trimEnd())
    .join('\n')
    .replace(/\n{3,}/g, '\n\n')
    .trim();
}

function extractNumberedItems(body) {
  const items = [];
  let current = null;

  for (const rawLine of body.split('\n')) {
    const numbered = /^\s*\d+\.\s+(.*)$/.exec(rawLine);
    if (numbered) {
      if (current !== null) {
        items.push(cleanPostBody(current));
      }
      current = numbered[1];
      continue;
    }

    if (current !== null && rawLine.trim()) {
      current = `${current}\n${rawLine}`;
    }
  }

  if (current !== null) {
    items.push(cleanPostBody(current));
  }

  return items.filter(Boolean);
}

function extractImagePaths(draftsXSection) {
  const images = new Set();
  const headingRegex = /^###\s+Images\b.*$/gm;
  let match;

  while ((match = headingRegex.exec(draftsXSection)) !== null) {
    const start = match.index + match[0].length;
    const nextHeading = /^###\s+/gm;
    nextHeading.lastIndex = start;
    const nextMatch = nextHeading.exec(draftsXSection);
    const body = draftsXSection.slice(start, nextMatch ? nextMatch.index : draftsXSection.length);
    const pathRegex = /resource\/[^\s`)"']+\.[A-Za-z0-9]+/g;
    let pathMatch;
    while ((pathMatch = pathRegex.exec(body)) !== null) {
      images.add(pathMatch[0].replace(/[.,;:]+$/, ''));
    }
  }

  return [...images];
}

function parseArtifact(markdown) {
  const draftsX = extractDraftsXSection(markdown);
  const singleBody = sectionBody(draftsX, /^###\s+Single post\s*$/m, /^###\s+/gm);
  const threadBody = sectionBody(draftsX, /^###\s+Thread outline\b.*$/m, /^###\s+/gm);
  const singlePost = cleanPostBody(singleBody);
  const threadPosts = extractNumberedItems(threadBody);
  const imagePaths = extractImagePaths(draftsX);

  if (!singlePost && threadPosts.length === 0) {
    throw new Error("Artifact has no X drafts under '### Single post' or '### Thread outline'.");
  }

  return {
    singlePost,
    threadPosts,
    imagePaths,
  };
}

function artifactDateFromPath(artifactPath) {
  const name = basename(artifactPath);
  const match = /^(\d{4}-\d{2}-\d{2})\.md$/.exec(name);
  return match ? match[1] : formatLocalDate(new Date());
}

function contentTypeForPath(imagePath) {
  const ext = extname(imagePath).toLowerCase();
  if (ext === '.png') {
    return 'image/png';
  }
  if (ext === '.jpg' || ext === '.jpeg') {
    return 'image/jpeg';
  }
  if (ext === '.webp') {
    return 'image/webp';
  }
  throw new Error(`Unsupported image extension for ${imagePath}. Supported: .png, .jpg, .jpeg, .webp.`);
}

function uploadPlanForImages(imagePaths, artifactDate, r2PublicBaseUrl) {
  return imagePaths.map((imagePath) => {
    const ext = extname(imagePath).toLowerCase();
    const stem = basename(imagePath, extname(imagePath));
    const key = `${artifactDate}/${stem}-${Date.now()}${ext}`;
    return {
      localPath: imagePath,
      key,
      contentType: contentTypeForPath(imagePath),
      publicUrl: `${r2PublicBaseUrl}/${key}`,
    };
  });
}

function assetsForUrls(imageUrls) {
  return imageUrls.map((url) => ({
    image: { url },
  }));
}

function buildSingleDraftInput(text, channelId, imageUrls) {
  return {
    text,
    channelId,
    schedulingType: 'automatic',
    mode: 'addToQueue',
    saveToDraft: true, // HARDCODED: never queue/schedule. Drafts only.
    assets: assetsForUrls(imageUrls),
  };
}

function buildThreadDraftInput(threadPosts, channelId, imageUrls, service) {
  const tail = threadPosts.slice(1).map((text) => ({ text, assets: [] }));
  return {
    text: threadPosts[0],
    channelId,
    schedulingType: 'automatic',
    mode: 'addToQueue',
    saveToDraft: true, // HARDCODED: never queue/schedule. Drafts only.
    assets: assetsForUrls(imageUrls),
    metadata: {
      [service]: { thread: tail },
    },
  };
}

function buildCreatePostRequest(input) {
  return {
    query: `
mutation CreateBufferDraft($input: CreatePostInput!) {
  createPost(input: $input) {
    __typename
    ... on PostActionSuccess {
      post {
        id
        text
      }
    }
    ... on MutationError {
      message
    }
  }
}
`,
    variables: { input },
  };
}

function buildDraftRequests(parsed, channelId, imageUrls, service) {
  const requests = [];

  if (parsed.singlePost) {
    requests.push({
      kind: `single post (${service})`,
      request: buildCreatePostRequest(buildSingleDraftInput(parsed.singlePost, channelId, imageUrls)),
    });
  }

  if (parsed.threadPosts.length > 0) {
    requests.push({
      kind: `thread (${service})`,
      threadDocUrl: BUFFER_THREAD_DOC_URL,
      request: buildCreatePostRequest(buildThreadDraftInput(parsed.threadPosts, channelId, imageUrls, service)),
    });
  }

  return requests;
}

function runProcess(command, args) {
  return new Promise((resolveProcess, reject) => {
    const child = spawn(command, args, {
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';

    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk.toString();
    });
    child.on('error', (error) => {
      reject(error);
    });
    child.on('close', (code) => {
      resolveProcess({ code, stdout, stderr });
    });
  });
}

async function uploadImage(upload, r2Bucket) {
  const localPath = resolve(upload.localPath);
  let result;
  try {
    result = await runProcess('wrangler', [
      'r2',
      'object',
      'put',
      `${r2Bucket}/${upload.key}`,
      `--file=${localPath}`,
      `--content-type=${upload.contentType}`,
      '--remote',
    ]);
  } catch (error) {
    fatal(`R2 upload failed for ${upload.localPath}:\n${error.message}`, 2);
  }

  if (result.code !== 0) {
    const output = [result.stdout, result.stderr].filter(Boolean).join('\n').trim();
    fatal(`R2 upload failed for ${upload.localPath}:\n${output}`, 2);
  }

  return upload.publicUrl;
}

async function createBufferDraft(draftRequest) {
  let response;
  try {
    response = await fetch(BUFFER_API_URL, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${bufferAccessToken}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(draftRequest.request),
    });
  } catch (error) {
    fatal(`Buffer API request failed: ${error.message}`, 3);
  }

  let payload;
  try {
    payload = await response.json();
  } catch (error) {
    const text = await response.text().catch(() => '');
    fatal(`Buffer API returned non-JSON response (${response.status}): ${text || error.message}`, 3);
  }

  if (!response.ok) {
    fatal(`Buffer API HTTP ${response.status}: ${JSON.stringify(payload)}`, 3);
  }

  if (payload.errors?.length) {
    fatal(`Buffer API GraphQL errors: ${JSON.stringify(payload.errors)}`, 3);
  }

  const result = payload.data?.createPost;
  if (!result) {
    fatal(`Buffer API response missing createPost payload: ${JSON.stringify(payload)}`, 3);
  }

  if (result.__typename === 'MutationError') {
    fatal(`Buffer API MutationError: ${result.message}`, 3);
  }

  if (result.__typename !== 'PostActionSuccess') {
    fatal(`Buffer API returned unexpected payload ${result.__typename}: ${JSON.stringify(result)}`, 3);
  }

  const id = result.post?.id;
  if (!id) {
    fatal(`Buffer API success response did not include a post id: ${JSON.stringify(result)}`, 3);
  }

  return {
    kind: draftRequest.kind,
    id,
    url: `https://publish.buffer.com/drafts/${id}`,
  };
}

function publishedSection(drafts) {
  const lines = ['## Published as drafts'];
  for (const draft of drafts) {
    lines.push(`- ${draft.kind} — Buffer draft id: ${draft.id} — UI URL: ${draft.url}`);
  }
  return `${lines.join('\n')}\n`;
}

async function appendPublishedSection(artifactPath, drafts) {
  const markdown = await readFile(artifactPath, 'utf8');
  const lines = drafts.map((draft) => `- ${draft.kind} — Buffer draft id: ${draft.id} — UI URL: ${draft.url}`);
  const existing = /^##\s+Published as drafts\s*$/m.exec(markdown);

  if (!existing) {
    const separator = markdown.endsWith('\n') ? '\n' : '\n\n';
    await writeFile(artifactPath, `${markdown}${separator}${publishedSection(drafts)}`, 'utf8');
    return;
  }

  const insertAtSearchStart = existing.index + existing[0].length;
  const tail = markdown.slice(insertAtSearchStart);
  const nextHeading = /^##\s+/m.exec(tail);
  const insertAt = nextHeading ? insertAtSearchStart + nextHeading.index : markdown.length;
  const prefix = markdown.slice(0, insertAt).replace(/\s*$/, '\n');
  const suffix = markdown.slice(insertAt);
  const joined = `${prefix}${lines.join('\n')}\n${suffix.startsWith('\n') || !suffix ? suffix : `\n${suffix}`}`;
  await writeFile(artifactPath, joined, 'utf8');
}

function printDryRunPlan(args, env, parsed, uploads, draftRequests) {
  const plan = {
    mode: 'dry-run',
    artifact: args.artifact,
    channelId: args.channelId,
    r2Bucket: env.r2Bucket,
    r2PublicBaseUrl: env.r2PublicBaseUrl,
    parsedPosts: {
      singlePost: parsed.singlePost || null,
      threadPosts: parsed.threadPosts,
      threadHandling: parsed.threadPosts.length > 0
        ? `linked via metadata.twitter.thread (${BUFFER_THREAD_DOC_URL})`
        : null,
    },
    uploads,
    createPostVariables: draftRequests.map((draft) => ({
      kind: draft.kind,
      variables: draft.request.variables,
    })),
  };

  console.log(redact(JSON.stringify(plan, null, 2)));
}

async function main() {
  const args = parseArgs(process.argv);
  const env = validateEnvironment();
  const artifactMarkdown = await readFile(args.artifact, 'utf8').catch((error) => {
    fatal(`Failed to read artifact ${args.artifact}: ${error.message}`, 1);
  });

  let parsed;
  try {
    parsed = parseArtifact(artifactMarkdown);
  } catch (error) {
    fatal(`Failed to parse artifact ${args.artifact}: ${error.message}`, 1);
  }

  const artifactDate = artifactDateFromPath(args.artifact);
  let uploads;
  try {
    uploads = uploadPlanForImages(parsed.imagePaths, artifactDate, env.r2PublicBaseUrl);
  } catch (error) {
    fatal(error.message, 1);
  }

  const plannedImageUrls = uploads.map((upload) => upload.publicUrl);
  const draftRequests = buildDraftRequests(parsed, args.channelId, plannedImageUrls, args.service);

  if (args.dryRun) {
    printDryRunPlan(args, env, parsed, uploads, draftRequests);
    return;
  }

  const imageUrls = [];
  for (const upload of uploads) {
    imageUrls.push(await uploadImage(upload, env.r2Bucket));
  }

  const finalDraftRequests = buildDraftRequests(parsed, args.channelId, imageUrls, args.service);
  const drafts = [];
  for (const draftRequest of finalDraftRequests) {
    drafts.push(await createBufferDraft(draftRequest));
  }

  await appendPublishedSection(args.artifact, drafts);

  for (const draft of drafts) {
    console.log(redact(`${draft.kind}: Buffer draft id ${draft.id} (${draft.url})`));
  }
}

main().catch((error) => {
  fatal(error?.stack || error?.message || error, 1);
});
