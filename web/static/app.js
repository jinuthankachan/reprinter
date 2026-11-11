'use strict';
/*
 * reprinter web interface. Plain JavaScript with no build step: reprinter-web
 * serves this file as-is. Three views, switched by the URL hash:
 *   #/print     upload a file, choose the page setup, print, watch the queue
 *   #/jobs      active and recent jobs on all printers
 *   #/settings  printers, adding printers, CUPS sharing, the web interface
 */

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const $ = (sel, root = document) => root.querySelector(sel);
const enc = encodeURIComponent;
const PROPS = new Set(['value', 'checked', 'selected', 'disabled', 'hidden', 'open', 'required']);

/** Creates an element. Strings become text nodes, so nothing is parsed as HTML. */
function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value == null || value === false) continue;
    if (key === 'class') el.className = value;
    else if (key.startsWith('on') && typeof value === 'function') el.addEventListener(key.slice(2), value);
    else if (PROPS.has(key)) el[key] = value;
    else el.setAttribute(key, value === true ? '' : value);
  }
  for (const child of children.flat(Infinity)) {
    if (child == null || child === false) continue;
    el.append(child instanceof Node ? child : String(child));
  }
  return el;
}

/** JSON API call. Throws an Error carrying the server's message and status. */
async function api(method, path, body) {
  const options = { method, headers: { 'X-Reprinter': '1' } };
  if (body !== undefined) {
    options.headers['Content-Type'] = 'application/json';
    options.body = JSON.stringify(body);
  }
  let response;
  try {
    response = await fetch('/api' + path, options);
  } catch {
    throw new Error('Cannot reach the print server.');
  }
  let data = null;
  try { data = await response.json(); } catch { /* empty body */ }
  if (!response.ok) {
    const error = new Error(data?.error || `${response.status} ${response.statusText}`);
    error.status = response.status;
    throw error;
  }
  return data;
}

/** Uploads with progress (fetch cannot report upload progress). */
function uploadFile(file, onProgress) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/api/uploads');
    xhr.setRequestHeader('X-Reprinter', '1');
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded / e.total);
    xhr.onload = () => {
      let data = null;
      try { data = JSON.parse(xhr.responseText); } catch { /* not JSON */ }
      if (xhr.status >= 200 && xhr.status < 300) resolve(data);
      else reject(new Error(data?.error || `Upload failed (${xhr.status})`));
    };
    xhr.onerror = () => reject(new Error('Upload failed: the connection was lost.'));
    const form = new FormData();
    form.append('file', file);
    xhr.send(form);
  });
}

/** Per-browser preferences. Storage may be unavailable (private mode). */
const prefs = {
  get(key, fallback) {
    try {
      const v = localStorage.getItem('reprinter.' + key);
      return v === null ? fallback : JSON.parse(v);
    } catch { return fallback; }
  },
  set(key, value) {
    try { localStorage.setItem('reprinter.' + key, JSON.stringify(value)); } catch { /* ignore */ }
  },
};

let toastTimer;
function toast(message, kind = 'ok') {
  const t = $('#toast');
  t.textContent = message;
  t.className = `toast show ${kind}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.className = 'toast'; }, kind === 'error' ? 8000 : 4000);
}

/** Runs an async action with a spinner on its button; errors become toasts. */
async function busy(button, action) {
  button.disabled = true;
  button.classList.add('busy');
  try {
    return await action();
  } catch (e) {
    toast(e.message, 'error');
  } finally {
    button.disabled = false;
    button.classList.remove('busy');
  }
}

const fmtSize = (bytes) =>
  bytes < 1024 ? `${bytes} B` : bytes < 1048576 ? `${Math.round(bytes / 1024)} KB` : `${(bytes / 1048576).toFixed(1)} MB`;
const fmtTime = (unix) =>
  unix ? new Date(unix * 1000).toLocaleString([], { dateStyle: 'short', timeStyle: 'short' }) : '';

const field = (label, control, hint) =>
  h('label', { class: 'field' }, h('span', {}, label), control, hint && h('small', {}, hint));
const toggle = (checked) => h('input', { type: 'checkbox', class: 'switch', checked });
const toggleRow = (input, title, description) =>
  h('label', { class: 'toggle' }, input, h('span', {}, h('strong', {}, title), description && h('small', {}, description)));

// ---------------------------------------------------------------------------
// Shared state and rendering
// ---------------------------------------------------------------------------

const state = {
  session: null,
  printers: [],
  printer: '',   // selected on the Print view
  ppd: [],       // its PPD options
  upload: null,  // the uploaded document
};

async function loadSession() {
  state.session = await api('GET', '/session');
  $('#host').textContent = state.session.hostname;
  document.title = `reprinter · ${state.session.hostname}`;
}

function statePill(printer) {
  if (!printer.accepting) return h('span', { class: 'pill stopped' }, 'Not accepting jobs');
  const label = { idle: 'Ready', printing: 'Printing', stopped: 'Stopped' }[printer.state] || printer.state;
  return h('span', { class: `pill ${printer.state}` }, label);
}

const JOB_STATES = {
  pending: 'Waiting', held: 'Held', printing: 'Printing', stopped: 'Stopped',
  canceled: 'Canceled', aborted: 'Failed', completed: 'Done',
};

function jobRow(job, { cancel = false, showPrinter = true } = {}) {
  const meta = [
    `#${job.id}`,
    showPrinter && job.printer,
    job.user,
    job.size_kb ? fmtSize(job.size_kb * 1024) : null,
    fmtTime(job.completed || job.created),
  ].filter(Boolean).join(' · ');
  return h('li', { class: 'job' },
    h('div', { class: 'job-main' }, h('strong', {}, job.name || `Job ${job.id}`), h('small', {}, meta)),
    h('span', { class: `pill ${job.state}` }, JOB_STATES[job.state] || job.state),
    cancel && h('button', { type: 'button', class: 'ghost small', onclick: (e) => cancelJob(e.currentTarget, job) }, 'Cancel'));
}

function jobList(jobs, options, empty) {
  return jobs.length
    ? h('ul', { class: 'jobs' }, jobs.map((j) => jobRow(j, options)))
    : h('p', { class: 'muted' }, empty);
}

async function cancelJob(button, job) {
  if (!confirm(`Cancel job ${job.id} (${job.name || 'untitled'})?`)) return;
  await busy(button, async () => {
    await api('DELETE', `/jobs/${job.id}`);
    toast(`Job ${job.id} canceled`);
    refreshCurrent();
  });
}

// PPD options: the printer's own settings. These keys get friendly labels and
// sit with the main settings; everything else goes under "More options".
const MAIN_PPD = ['PageSize', 'Duplex', 'InputSlot', 'MediaType', 'ColorModel'];
const PPD_LABELS = {
  PageSize: 'Paper size', Duplex: 'Two-sided', InputSlot: 'Paper source', MediaType: 'Paper type', ColorModel: 'Color',
};
const CHOICE_LABELS = {
  Duplex: { None: 'Off', DuplexNoTumble: 'Long edge (book)', DuplexTumble: 'Short edge (notepad)' },
};

function ppdField(option) {
  const labels = CHOICE_LABELS[option.key] || {};
  const select = h('select', { 'data-ppd': option.key },
    option.choices.map((c) => h('option', { value: c, selected: c === option.default }, labels[c] || c)));
  return field(PPD_LABELS[option.key] || option.label || option.key, select);
}

// ---------------------------------------------------------------------------
// Print view
// ---------------------------------------------------------------------------

const timers = {};

async function enterPrint() {
  await loadPrinters();
  refreshQueue();
  timers.print = setInterval(() => {
    if (document.hidden) return;
    loadPrinters(true);
    refreshQueue();
  }, 5000);
}

async function loadPrinters(quiet = false) {
  let list;
  try {
    list = await api('GET', '/printers');
  } catch (e) {
    if (!quiet) toast(e.message, 'error');
    return;
  }
  state.printers = list;
  $('#no-printers').hidden = list.length > 0;

  const select = $('#printer');
  const names = list.map((p) => p.name).join('\n');
  if (select.dataset.names !== names) {
    select.replaceChildren(...list.map((p) =>
      h('option', { value: p.name }, p.info && p.info !== p.name ? `${p.name} — ${p.info}` : p.name)));
    select.dataset.names = names;
  }
  const wanted = state.printer || prefs.get('printer', '');
  const pick = list.find((p) => p.name === wanted) || list.find((p) => p.is_default) || list[0];
  if (pick) {
    select.value = pick.name;
    if (pick.name !== state.printer) await selectPrinter(pick.name);
  } else {
    state.printer = '';
  }
  showPrinterStatus();
  updatePrintButton();
}

async function selectPrinter(name) {
  state.printer = name;
  prefs.set('printer', name);
  state.ppd = [];
  renderPpd();
  showPrinterStatus();
  try {
    state.ppd = await api('GET', `/printers/${enc(name)}/options`);
  } catch (e) {
    toast(e.message, 'error');
  }
  if (state.printer === name) renderPpd();
  refreshQueue();
}

function showPrinterStatus() {
  const p = state.printers.find((x) => x.name === state.printer);
  $('#printer-state').replaceChildren(p ? statePill(p) : '');
  $('#printer-message').textContent = p ? (p.state_message || [p.make_model, p.location].filter(Boolean).join(' · ')) : '';
}

function renderPpd() {
  const main = MAIN_PPD.map((k) => state.ppd.find((o) => o.key === k)).filter(Boolean);
  const extra = state.ppd.filter((o) => !MAIN_PPD.includes(o.key));
  $('#ppd-main').replaceChildren(...main.map(ppdField));
  $('#ppd-extra').replaceChildren(...extra.map(ppdField));
  // A printer that cannot print both sides gets the by-hand option instead.
  const duplex = state.ppd.find((o) => o.key === 'Duplex');
  const hasDuplex = !!duplex && duplex.choices.length > 1;
  $('#manual-duplex-row').hidden = hasDuplex;
  if (hasDuplex) $('#manual-duplex').checked = false;
  syncSetup();
}

const manualDuplex = () => $('#manual-duplex').checked && !$('#manual-duplex-row').hidden;

function syncSetup() {
  const manual = manualDuplex();
  const copies = $('#copies');
  copies.disabled = manual;
  if (manual) copies.value = 1;
  $('#collate-row').hidden = manual || (Number(copies.value) || 1) <= 1;
  $('#page-set').disabled = manual;
  $('#print-btn').textContent = manual ? 'Print front sides' : 'Print';
  updatePrintButton();
}

function updatePrintButton() {
  const p = state.printers.find((x) => x.name === state.printer);
  $('#print-btn').disabled = !state.upload || !p || !p.accepting;
}

function setupPrintView() {
  $('#printer').addEventListener('change', (e) => selectPrinter(e.target.value));
  $('#copies').addEventListener('input', syncSetup);
  $('#manual-duplex').addEventListener('change', () => {
    $('#duplex-panel').hidden = true;
    syncSetup();
  });
  $('#setup').addEventListener('submit', submitPrint);
  $('#doc-remove').addEventListener('click', removeUpload);

  const drop = $('#drop');
  const input = $('#file');
  input.addEventListener('change', () => input.files[0] && handleFile(input.files[0]));
  // Accept a file dropped anywhere on the Print view, not just on the box.
  const view = $('#view-print');
  for (const type of ['dragenter', 'dragover']) {
    view.addEventListener(type, (e) => {
      if (![...(e.dataTransfer?.types || [])].includes('Files')) return;
      e.preventDefault();
      drop.classList.add('over');
    });
  }
  view.addEventListener('dragleave', (e) => { if (!view.contains(e.relatedTarget)) drop.classList.remove('over'); });
  view.addEventListener('drop', (e) => {
    e.preventDefault();
    drop.classList.remove('over');
    const file = e.dataTransfer?.files?.[0];
    if (file) handleFile(file);
  });
}

async function handleFile(file) {
  const maxMb = state.session?.max_upload_mb || 50;
  if (file.size > maxMb * 1048576) {
    toast(`${file.name} is larger than the ${maxMb} MB upload limit.`, 'error');
    return;
  }
  await removeUpload();
  const progress = $('#progress');
  const bar = $('#progress-bar');
  bar.style.width = '0%';
  progress.hidden = false;
  $('#drop').hidden = true;
  try {
    state.upload = await uploadFile(file, (fraction) => { bar.style.width = `${Math.round(fraction * 100)}%`; });
    showDocument();
  } catch (e) {
    toast(e.message, 'error');
    $('#drop').hidden = false;
  } finally {
    progress.hidden = true;
    $('#file').value = '';
    updatePrintButton();
  }
}

const KIND_LABELS = { pdf: 'PDF', postscript: 'PostScript', image: 'Image', text: 'Text' };

function showDocument() {
  const u = state.upload;
  const url = `/api/uploads/${u.id}`;
  $('#doc-name').textContent = u.name;
  $('#doc-meta').textContent = `${KIND_LABELS[u.kind] || u.kind} · ${fmtSize(u.size)}`;
  let preview;
  if (u.kind === 'pdf') {
    // Phones without a built-in PDF viewer would download an embedded PDF.
    preview = navigator.pdfViewerEnabled
      ? h('iframe', { src: url, title: `Preview of ${u.name}` })
      : h('a', { href: url, target: '_blank', rel: 'noopener', class: 'open-link' }, 'Open the PDF to check it');
  } else if (u.kind === 'image') {
    preview = h('img', { src: url, alt: `Preview of ${u.name}` });
  } else if (u.kind === 'text') {
    preview = h('pre', {}, 'Loading…');
    textStart(url).then((t) => { preview.textContent = t; }).catch(() => { preview.textContent = 'No preview.'; });
  } else {
    preview = h('p', { class: 'muted' }, 'No preview for PostScript files.');
  }
  $('#preview').replaceChildren(preview);
  $('#doc').hidden = false;
  $('#duplex-panel').hidden = true;
}

/** The first part of a text file, without downloading all of it. */
async function textStart(url, limit = 20000) {
  const response = await fetch(url);
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let text = '';
  while (text.length < limit) {
    const { done, value } = await reader.read();
    if (done) return text;
    text += decoder.decode(value, { stream: true });
  }
  reader.cancel();
  return text.slice(0, limit) + '\n…';
}

async function removeUpload() {
  const u = state.upload;
  state.upload = null;
  $('#doc').hidden = true;
  $('#preview').replaceChildren();
  $('#drop').hidden = false;
  $('#duplex-panel').hidden = true;
  updatePrintButton();
  if (u) api('DELETE', `/uploads/${u.id}`).catch(() => {});
}

function printRequest(overrides = {}) {
  const options = {};
  for (const select of document.querySelectorAll('#setup select[data-ppd]')) options[select.dataset.ppd] = select.value;
  return {
    upload_id: state.upload.id,
    printer: state.printer,
    title: state.upload.name,
    copies: Math.max(1, Math.min(999, Number($('#copies').value) || 1)),
    collate: $('#collate').checked,
    page_ranges: $('#pages').value,
    page_set: $('#page-set').value,
    reverse: $('#reverse').checked,
    orientation: $('#orientation').value,
    scaling: $('#scaling').value,
    number_up: Number($('#number-up').value),
    options,
    ...overrides,
  };
}

async function submitPrint(event) {
  event.preventDefault();
  if (!state.upload) return;
  const manual = manualDuplex();
  const request = printRequest(manual ? { page_set: 'odd', copies: 1 } : {});
  await busy($('#print-btn'), async () => {
    const result = await api('POST', '/print', request);
    if (manual) {
      showBackSidesStep(request, result.job_id);
    } else {
      toast(`Sent to ${result.printer} as job ${result.job_id}`);
    }
    refreshQueue();
  });
  updatePrintButton();
}

/** Manual two-sided printing, step 2: the user reloads the paper. */
function showBackSidesStep(frontRequest, frontJob) {
  const panel = $('#duplex-panel');
  const reverse = h('input', { type: 'checkbox', checked: prefs.get('duplexReverse', false) });
  const print = h('button', { type: 'button', class: 'primary' }, 'Print back sides');
  print.addEventListener('click', () => busy(print, async () => {
    prefs.set('duplexReverse', reverse.checked);
    const result = await api('POST', '/print', { ...frontRequest, page_set: 'even', reverse: reverse.checked });
    panel.hidden = true;
    toast(`Back sides sent as job ${result.job_id}`);
    refreshQueue();
  }));
  panel.replaceChildren(
    h('h3', {}, 'Front sides sent (job ' + frontJob + ')'),
    h('ol', {},
      h('li', {}, 'Wait until the front sides have finished printing.'),
      h('li', {}, 'Put the printed sheets back into the paper tray, turned over so the blank sides get printed.'),
      h('li', {}, 'If the document has an odd number of pages, take out the last sheet first.')),
    h('label', { class: 'check' }, reverse,
      h('span', {}, 'Print back sides in reverse order', h('small', {}, 'Tick this if the back sides came out in the wrong order last time.'))),
    h('div', { class: 'actions' }, print,
      h('button', { type: 'button', class: 'ghost', onclick: () => { panel.hidden = true; } }, 'Cancel')));
  panel.hidden = false;
  panel.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
}

async function refreshQueue() {
  if (!state.printer) {
    $('#queue').replaceChildren(h('p', { class: 'muted' }, 'No printer selected.'));
    return;
  }
  try {
    const data = await api('GET', `/jobs?printer=${enc(state.printer)}&history=false`);
    $('#queue').replaceChildren(jobList(data.active, { cancel: true, showPrinter: false }, 'No jobs waiting.'));
  } catch { /* the printer list shows connection problems */ }
}

// ---------------------------------------------------------------------------
// Jobs view
// ---------------------------------------------------------------------------

async function enterJobs() {
  const select = $('#jobs-printer');
  try {
    const printers = await api('GET', '/printers');
    const current = select.value;
    select.replaceChildren(h('option', { value: '' }, 'All printers'), ...printers.map((p) => h('option', { value: p.name }, p.name)));
    select.value = printers.some((p) => p.name === current) ? current : '';
  } catch (e) {
    toast(e.message, 'error');
  }
  select.onchange = renderJobs;
  await renderJobs();
  timers.jobs = setInterval(() => { if (!document.hidden) renderJobs(); }, 5000);
}

async function renderJobs() {
  const printer = $('#jobs-printer').value;
  let data;
  try {
    data = await api('GET', printer ? `/jobs?printer=${enc(printer)}` : '/jobs');
  } catch (e) {
    $('#jobs-active').replaceChildren(h('p', { class: 'notice error' }, e.message));
    return;
  }
  $('#jobs-active').replaceChildren(jobList(data.active, { cancel: true }, 'No active jobs.'));
  $('#jobs-finished').replaceChildren(jobList(data.finished, {}, 'No finished jobs yet.'));
}

// ---------------------------------------------------------------------------
// Settings view
// ---------------------------------------------------------------------------

async function renderSettings() {
  const root = $('#view-settings');
  const open = new Set([...root.querySelectorAll('details.printer[open]')].map((d) => d.dataset.name));
  if (!root.children.length) root.replaceChildren(h('p', { class: 'muted pad' }, 'Loading…'));
  try {
    await loadSession();
    if (state.session.password_set && !state.session.admin) {
      root.replaceChildren(loginForm());
      return;
    }
    const [settings, printers] = await Promise.all([api('GET', '/admin/settings'), api('GET', '/printers')]);
    const warning = !settings.web.password_set && h('div', { class: 'notice warn' },
      'Anyone who can open this page can change these settings. ',
      h('button', { type: 'button', class: 'link', onclick: () => $('#web-settings').scrollIntoView({ behavior: 'smooth' }) },
        'Set an admin password'), '.');
    root.replaceChildren(...[
      warning,
      printersCard(printers, open),
      addPrinterCard(printers),
      cupsCard(settings.cups),
      webCard(settings.web),
    ].filter(Boolean));
  } catch (e) {
    if (e.status === 401) {
      state.session.admin = false;
      root.replaceChildren(loginForm());
    } else {
      root.replaceChildren(h('div', { class: 'notice error' }, e.message));
    }
  }
}

function loginForm() {
  const password = h('input', { type: 'password', autocomplete: 'current-password', required: true });
  const submit = h('button', { type: 'submit', class: 'primary wide' }, 'Log in');
  const form = h('form', { class: 'card narrow' },
    h('h2', {}, 'Log in to change settings'),
    h('p', { class: 'muted' }, 'The settings are protected by an admin password. Printing does not need it.'),
    h('div', { class: 'fields' }, field('Password', password)),
    submit);
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    busy(submit, async () => {
      await api('POST', '/login', { password: password.value });
      await renderSettings();
    });
  });
  setTimeout(() => password.focus(), 0);
  return form;
}

// Printers ------------------------------------------------------------------

function printersCard(printers, open) {
  return h('section', { class: 'card' },
    h('h2', {}, 'Printers'),
    printers.length
      ? h('div', { class: 'printer-list' }, printers.map((p) => printerDetails(p, open.has(p.name))))
      : h('p', { class: 'muted' }, 'No printers yet. Add one below.'));
}

function printerDetails(p, open) {
  const body = h('div', { class: 'printer-body' }, h('p', { class: 'muted' }, 'Loading…'));
  const details = h('details', { class: 'printer', 'data-name': p.name },
    h('summary', {},
      h('span', { class: 'printer-sum' }, h('strong', {}, p.name), h('small', {}, p.info || p.make_model)),
      h('span', { class: 'badges' },
        p.is_default && h('span', { class: 'pill' }, 'Default'),
        p.shared && h('span', { class: 'pill' }, 'Shared'),
        statePill(p))),
    body);
  let loaded = false;
  const load = async () => {
    if (loaded) return;
    loaded = true;
    body.replaceChildren(await printerForm(p));
  };
  details.addEventListener('toggle', () => details.open && load());
  if (open) details.open = true;
  return details;
}

async function printerForm(p) {
  let ppd = [];
  try {
    ppd = await api('GET', `/printers/${enc(p.name)}/options`);
  } catch (e) {
    toast(e.message, 'error');
  }
  const info = h('input', { type: 'text', value: p.info, maxlength: 127 });
  const location = h('input', { type: 'text', value: p.location, maxlength: 127 });
  const uri = h('input', { type: 'text', value: p.device_uri, spellcheck: 'false', autocomplete: 'off' });
  const shared = toggle(p.shared);
  const accepting = toggle(p.accepting);
  const enabled = toggle(p.state !== 'stopped');
  const defaults = h('div', { class: 'fields' }, ppd.map(ppdField));
  const devices = h('div');

  const detect = h('button', { type: 'button', class: 'ghost small' }, 'Detect');
  detect.addEventListener('click', () => busy(detect, async () => {
    const list = await api('GET', `/admin/printers/${enc(p.name)}/devices`);
    devices.replaceChildren(list.length
      ? h('ul', { class: 'device-list' }, list.map((d) => h('li', {},
        h('button', { type: 'button', class: 'link', onclick: () => { uri.value = d.uri; } },
          d.make_model || d.info || d.uri, d.matches ? ' (current)' : ''),
        h('small', {}, d.uri))))
      : h('p', { class: 'hint' }, 'No devices found. Is the printer plugged in and switched on?'));
  }));

  const save = h('button', { type: 'submit', class: 'primary' }, 'Save');
  const form = h('form', {},
    h('div', { class: 'fields' }, field('Description', info), field('Location', location)),
    h('div', { class: 'field' }, h('span', {}, 'Device URI'), h('div', { class: 'row' }, uri, detect)),
    devices,
    h('div', { class: 'toggles' },
      toggleRow(shared, 'Shared on the network', 'Other devices can print to it when network sharing is on (below).'),
      toggleRow(accepting, 'Accepting jobs', 'Off: new jobs are refused.'),
      toggleRow(enabled, 'Enabled', 'Off: jobs wait in the queue until it is enabled again.')),
    ppd.length > 0 && h('h3', {}, 'Default options'),
    ppd.length > 0 && h('p', { class: 'hint' }, 'Used for jobs from every device unless the job says otherwise.'),
    defaults,
    h('div', { class: 'actions' }, save,
      !p.is_default && actionButton('Make default', 'ghost', async () => {
        await api('PUT', `/admin/printers/${enc(p.name)}`, { make_default: true });
        toast(`${p.name} is now the default printer`);
        renderSettings();
      }),
      actionButton('Print test page', 'ghost', async () => {
        const r = await api('POST', `/admin/printers/${enc(p.name)}/test-page`);
        toast(`Test page sent as job ${r.job_id}`);
      }),
      actionButton('Delete', 'danger', async () => {
        if (!confirm(`Delete the printer ${p.name}? Jobs waiting in its queue are canceled.`)) return;
        await api('DELETE', `/admin/printers/${enc(p.name)}`);
        toast(`Deleted ${p.name}`);
        renderSettings();
      })));

  form.addEventListener('submit', (e) => {
    e.preventDefault();
    busy(save, async () => {
      const changed = {};
      for (const select of defaults.querySelectorAll('select[data-ppd]')) {
        const option = ppd.find((o) => o.key === select.dataset.ppd);
        if (option && select.value !== option.default) changed[option.key] = select.value;
      }
      await api('PUT', `/admin/printers/${enc(p.name)}`, {
        info: info.value,
        location: location.value,
        device_uri: uri.value.trim(),
        shared: shared.checked,
        accepting: accepting.checked,
        enabled: enabled.checked,
        defaults: changed,
      });
      toast(`Saved ${p.name}`);
      renderSettings();
    });
  });
  return form;
}

function actionButton(label, kind, action) {
  const button = h('button', { type: 'button', class: kind }, label);
  button.addEventListener('click', () => busy(button, action));
  return button;
}

// Adding printers -----------------------------------------------------------

const PAPERS = [['a4', 'A4'], ['letter', 'US Letter'], ['legal', 'US Legal'], ['a5', 'A5'], ['b5', 'B5'], ['executive', 'Executive']];

function addPrinterCard(printers) {
  const body = h('div', {}, h('p', { class: 'muted' }, 'Looking for supported printers…'));
  api('GET', '/admin/profiles').then(({ profiles, source }) => {
    body.replaceChildren(...(profiles.length
      ? [h('p', { class: 'hint' }, 'Printers reprinter has a profile for. Plug the printer in and switch it on first.'),
         h('ul', { class: 'profiles' }, profiles.map((pr) => profileRow(pr, printers)))]
      : [h('p', { class: 'muted' }, `No printer profiles found (${source}).`)]));
  }).catch((e) => body.replaceChildren(h('p', { class: 'notice error' }, e.message)));
  return h('section', { class: 'card' }, h('h2', {}, 'Add a printer'), body);
}

function profileRow(pr, printers) {
  const slot = h('div');
  const setup = h('button', { type: 'button', class: 'ghost small', disabled: !pr.driver_installed }, 'Set up');
  setup.addEventListener('click', () => {
    setup.hidden = true;
    slot.replaceChildren(newPrinterForm(pr, printers, () => {
      setup.hidden = false;
      slot.replaceChildren();
    }));
  });
  return h('li', { class: 'profile' },
    h('div', { class: 'profile-head' },
      h('div', {}, h('strong', {}, pr.model), h('small', {}, `${pr.id} · driver ${pr.driver}`)),
      h('span', { class: 'badges' },
        h('span', { class: `pill ${pr.connected ? 'ok' : ''}` }, pr.connected ? 'Connected' : 'Not connected'),
        pr.queue_exists && h('span', { class: 'pill' }, `Queue ${pr.queue} exists`),
        !pr.driver_installed && h('span', { class: 'pill warn' }, 'Driver not installed')),
      setup),
    !pr.driver_installed && h('p', { class: 'hint' },
      'Install the driver first. On the server: ', h('code', {}, `sudo reprinter-web install --printer ${pr.id}`)),
    slot);
}

function newPrinterForm(pr, printers, close) {
  const name = h('input', { type: 'text', value: pr.queue, maxlength: 127, spellcheck: 'false' });
  const exists = h('p', { class: 'hint warn-text' }, 'A printer with this name exists; it will be updated in place (its jobs are kept).');
  const checkName = () => { exists.hidden = !printers.some((p) => p.name === name.value.trim()); };
  name.addEventListener('input', checkName);
  checkName();

  const device = h('select', {});
  const manual = h('input', { type: 'text', placeholder: `${pr.uri_scheme}://…`, spellcheck: 'false', autocomplete: 'off' });
  const status = h('small', {}, 'Detecting…');
  const syncDevice = () => { manual.hidden = device.value !== ''; };
  device.addEventListener('change', syncDevice);
  const detect = h('button', { type: 'button', class: 'ghost small' }, 'Detect again');
  const runDetect = () => busy(detect, async () => {
    status.textContent = 'Detecting…';
    let list = [];
    try {
      list = await api('GET', `/admin/profiles/${enc(pr.id)}/devices`);
      status.textContent = list.some((d) => d.matches)
        ? 'Found the printer.'
        : list.length ? 'No device matches this model. Pick one, or enter its URI.' : 'No device found. Is the printer plugged in and switched on?';
    } catch (e) {
      status.textContent = e.message;
    }
    device.replaceChildren(
      ...list.map((d) => h('option', { value: d.uri }, `${d.matches ? '✓ ' : ''}${d.make_model || d.info || d.uri}`)),
      h('option', { value: '' }, 'Enter a device URI by hand…'));
    syncDevice();
  });
  detect.addEventListener('click', runDetect);

  const paper = h('select', {}, PAPERS.map(([value, label]) => h('option', { value, selected: value === 'a4' }, label)));
  const shared = toggle(true);
  const create = h('button', { type: 'submit', class: 'primary' }, 'Add printer');
  const form = h('form', { class: 'new-printer' },
    h('div', { class: 'fields' }, field('Queue name', name, 'Letters, digits, _ . -'), field('Default paper', paper)),
    exists,
    h('div', { class: 'field' }, h('span', {}, 'Device'), h('div', { class: 'row' }, device, detect), status),
    manual,
    h('div', { class: 'toggles' }, toggleRow(shared, 'Share on the network')),
    h('div', { class: 'actions' }, create, h('button', { type: 'button', class: 'ghost', onclick: close }, 'Cancel')));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    busy(create, async () => {
      const deviceUri = (device.value || manual.value).trim();
      if (!deviceUri) throw new Error('Choose the printer from the device list, or enter its device URI.');
      await api('POST', '/admin/printers', {
        profile: pr.id, name: name.value.trim(), device_uri: deviceUri, paper: paper.value, shared: shared.checked,
      });
      toast(`Added ${name.value.trim()}`);
      renderSettings();
    });
  });
  runDetect();
  return form;
}

// CUPS network sharing ------------------------------------------------------

function cupsCard(c) {
  const share = toggle(c.share_printers);
  const any = toggle(c.remote_any);
  const admin = toggle(c.remote_admin);
  const save = h('button', { type: 'submit', class: 'primary' }, 'Save');
  const problem = h('div', { class: 'notice warn', hidden: true });
  const form = h('form', { class: 'card' },
    h('h2', {}, 'Network sharing'),
    h('p', { class: 'hint' }, 'CUPS server settings. Saving restarts CUPS; jobs that are printing carry on.'),
    h('div', { class: 'toggles' },
      toggleRow(share, 'Share printers', 'Shared printers are advertised on the network (AirPrint, IPP Everywhere, Mopria).'),
      toggleRow(any, 'Allow printing from any network', 'Off: only devices on the local subnet can print.'),
      toggleRow(admin, 'Remote CUPS administration', 'Allow the CUPS web interface on port 631 to be administered from other machines.')),
    problem,
    h('div', { class: 'actions' }, save));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    problem.hidden = true;
    busy(save, async () => {
      try {
        await api('PUT', '/admin/cups', { share_printers: share.checked, remote_any: any.checked, remote_admin: admin.checked });
      } catch (err) {
        if (err.status !== 403) throw err;
        // This CUPS only takes these settings from root: show the command to run.
        const at = err.message.indexOf('run: ');
        problem.replaceChildren(...(at < 0
          ? [err.message]
          : [err.message.slice(0, at + 5), h('code', {}, err.message.slice(at + 5))]));
        problem.hidden = false;
        return;
      }
      toast('Network sharing saved');
    });
  });
  return form;
}

// Web interface and password ------------------------------------------------

function webCard(w) {
  const maxMb = h('input', { type: 'number', min: 1, max: 1024, value: w.max_upload_mb, inputmode: 'numeric' });
  const ttl = h('input', { type: 'number', min: 5, max: 10080, value: w.upload_ttl_minutes, inputmode: 'numeric' });
  const listen = h('input', { type: 'text', value: w.listen, spellcheck: 'false' });
  const save = h('button', { type: 'submit', class: 'primary' }, 'Save');
  const settingsForm = h('form', {},
    h('div', { class: 'fields' },
      field('Upload limit (MB)', maxMb),
      field('Keep uploads for (minutes)', ttl, 'Unprinted files are deleted after this.'),
      field('Listen address', listen, `Takes effect after a restart (now ${w.running_listen}).`)),
    h('div', { class: 'actions' }, save));
  settingsForm.addEventListener('submit', (e) => {
    e.preventDefault();
    busy(save, async () => {
      const r = await api('PUT', '/admin/web', {
        listen: listen.value.trim(), max_upload_mb: Number(maxMb.value), upload_ttl_minutes: Number(ttl.value),
      });
      toast(r.restart_required ? 'Saved. Restart reprinter-web to use the new listen address.' : 'Saved');
      loadSession();
    });
  });

  const pw1 = h('input', { type: 'password', autocomplete: 'new-password', minlength: 6 });
  const pw2 = h('input', { type: 'password', autocomplete: 'new-password' });
  const pwSave = h('button', { type: 'submit', class: 'primary' }, w.password_set ? 'Change password' : 'Set password');
  const passwordForm = h('form', {},
    h('div', { class: 'fields' }, field(w.password_set ? 'New password' : 'Password', pw1, 'At least 6 characters.'), field('Repeat', pw2)),
    h('div', { class: 'actions' }, pwSave,
      w.password_set && actionButton('Remove password', 'ghost', async () => {
        if (!confirm('Remove the admin password? Anyone who can open this page will be able to change settings.')) return;
        await api('PUT', '/admin/password', { password: '' });
        toast('Password removed');
        renderSettings();
      }),
      w.password_set && actionButton('Log out', 'ghost', async () => {
        await api('POST', '/logout');
        renderSettings();
      })));
  passwordForm.addEventListener('submit', (e) => {
    e.preventDefault();
    busy(pwSave, async () => {
      if (pw1.value.length < 6) throw new Error('Use at least 6 characters.');
      if (pw1.value !== pw2.value) throw new Error('The two passwords do not match.');
      await api('PUT', '/admin/password', { password: pw1.value });
      toast('Password saved');
      renderSettings();
    });
  });

  return h('section', { class: 'card', id: 'web-settings' },
    h('h2', {}, 'Web interface'),
    settingsForm,
    h('h3', {}, 'Admin password'),
    h('p', { class: 'hint' }, w.password_set
      ? 'Changing settings needs the password. Printing and canceling jobs do not.'
      : 'Without a password, anyone who can open this page can change settings.'),
    passwordForm);
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

const views = {
  print: { enter: enterPrint, leave: () => clearInterval(timers.print) },
  jobs: { enter: enterJobs, leave: () => clearInterval(timers.jobs) },
  settings: { enter: renderSettings, leave: () => {} },
};
let currentView = null;

function route() {
  const name = location.hash.replace(/^#\/?/, '');
  const next = views[name] ? name : 'print';
  if (next === currentView) return;
  if (currentView) views[currentView].leave();
  currentView = next;
  for (const view of Object.keys(views)) $(`#view-${view}`).hidden = view !== next;
  for (const link of document.querySelectorAll('.tabs a')) {
    const active = link.dataset.view === next;
    link.classList.toggle('active', active);
    if (active) link.setAttribute('aria-current', 'page');
    else link.removeAttribute('aria-current');
  }
  views[next].enter();
}

function refreshCurrent() {
  if (currentView === 'print') refreshQueue();
  else if (currentView === 'jobs') renderJobs();
}

document.addEventListener('DOMContentLoaded', async () => {
  setupPrintView();
  try {
    await loadSession();
  } catch (e) {
    toast(e.message, 'error');
  }
  window.addEventListener('hashchange', route);
  route();
});
