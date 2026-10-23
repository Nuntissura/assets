import init, { BrowserPilot } from '/pkg/kumihimo_web.js';
import { BrowserAdapter } from './adapter.mjs';

const $ = selector => document.querySelector(selector);
const list = value => Array.isArray(value) ? value : Object.values(value ?? {});
let adapter;
let selectedFrame;
let editorTask;
let popupBefore;
const controls = { frame: $('#frame-select'), destination: $('#destination-select'), view: $('#view-select') };
function options(select, records, current, label) {
  const existing = new Map([...select.options].map(option => [option.value, option]));
  const wanted = new Set();
  for (const record of records) {
    wanted.add(record.id);
    let option = existing.get(record.id);
    if (!option) { option = document.createElement('option'); option.value = record.id; select.append(option); }
    option.textContent = label(record);
  }
  for (const [id, option] of existing) if (!wanted.has(id)) option.remove();
  if (wanted.has(current)) select.value = current;
}
function taskFor(view, host) {
  return list(host.tasks).find(task => list(task.views).some(item => (typeof item === 'string' ? item : item.id) === view));
}
function updateInspector(snapshot, host) {
  const frames = list(snapshot.frames);
  if (!frames.some(frame => frame.id === selectedFrame)) selectedFrame = frames[0]?.id;
  options(controls.frame, frames, selectedFrame, frame => `Frame ${frame.id}${frame.visible ? '' : ' · hidden'}${frame.expanded ? '' : ' · collapsed'}`);
  selectedFrame = controls.frame.value;
  const destination = controls.destination.value;
  const workspaces = list(snapshot.workspaces).filter(workspace => workspace.owner !== selectedFrame);
  options(controls.destination, workspaces, destination, workspace => `Workspace ${workspace.id}${workspace.owner ? ` · in frame ${workspace.owner}` : ' · root'}`);
  const frame = frames.find(frame => frame.id === selectedFrame);
  const allViews = list(snapshot.views).filter(view => !view.closed);
  const selectedView = controls.view.value;
  options(controls.view, allViews, selectedView, view => `${view.label ?? `View ${view.id}`} · ${view.id}`);
  if (!selectedView || !allViews.some(view => view.id === selectedView)) controls.view.value = frame?.selected_view ?? allViews[0]?.id ?? '';
  const task = taskFor(controls.view.value, host);
  if (task && task.id !== editorTask) {
    $('#task-title').value = task.title; $('#task-body').value = task.body;
    $('#task-column').value = task.column; $('#task-status').value = task.status;
    editorTask = task.id;
  }
  for (const id of ['#task-title', '#task-body', '#task-column', '#task-status']) $(id).disabled = !task;
  $('#host-state').textContent = task ? `Task ${task.id} · host revision ${host.revision} · ${task.column} / ${task.status} · veto ${task.veto ? 'on' : 'off'}` : 'Choose a host task view to edit.';
  $('#identity').textContent = `Generation ${snapshot.generation} · incarnation ${snapshot.identity?.incarnation ?? snapshot.identity?.session ?? JSON.stringify(snapshot.identity)}`;
  const split = list(snapshot.nodes).find(node => node.kind === 'split' && node.workspace === frame?.workspace);
  $('#resize-weight').disabled = !split || split.weights.length < 2;
  if (split && document.activeElement !== $('#resize-weight')) $('#resize-weight').value = Math.round(split.weights[0] / (split.weights[0] + split.weights[1]) * 100);
}
function selected() {
  const snapshot = adapter.current;
  const frame = list(snapshot.frames).find(item => item.id === controls.frame.value);
  const workspace = list(snapshot.workspaces).find(item => item.id === controls.destination.value);
  const destination = list(snapshot.frames).find(item => item.workspace === workspace?.id && item.id !== frame?.id);
  const view = list(snapshot.views).find(item => item.id === controls.view.value);
  return { snapshot, frame, destination, workspace, view, task: taskFor(view?.id, adapter.hostCurrent) };
}
function previewCommit(action) { const result = adapter.preview(action); return result.ok ? adapter.commitPreview() : result; }
function config() {
  return { task_count: Number($('#task-count').value), detail_count: Number($('#detail-count').value), nesting_depth: Number($('#nesting-depth').value), max_records: Number($('#max-records').value), width: 1100, height: 700 };
}
function expose(host) {
  window.kumihimo = {
    host, adapter, ready: false, events: adapter.events, storage: adapter.storage,
    route(request) {
      const result = adapter.route(request);
      if (['command', 'host_command', 'load'].includes(request.route)) adapter.refresh();
      return result;
    },
    snapshot: () => adapter.snapshot(), hostSnapshot: () => adapter.hostSnapshot(), popup: null,
    capabilities: { rustWasm: true, storage: 'host-selected localStorage', input: 'browser pointer/keyboard', nativePopupMigration: 'unsupported', physicalInput: 'unproven', otherBrowsers: 'unproven' },
  };
}
function createWorkspace(restore = false) {
  const host = new BrowserPilot(crypto.randomUUID(), JSON.stringify(config()));
  const previous = adapter;
  adapter = new BrowserAdapter({ host, root: $('#workspace'), status: $('#status'), onSnapshot: updateInspector,
    fileDrops: {} });
  previous?.dispose();
  // Retired DOM nodes are presentation only; the new host supplies all canonical state.
  if (previous) for (const node of previous.frames.values()) node.remove();
  if (previous) for (const node of previous.handles.values()) node.remove();
  selectedFrame = undefined; editorTask = undefined;
  expose(host);
  if (restore) adapter.load();
  adapter.whenIdle().then(() => { window.kumihimo.ready = true; $('#capabilities').textContent = 'Rust/WASM · browser pointer + keyboard · localStorage policy · popup migration unsupported'; });
  previous?.host.free?.();
}
document.addEventListener('click', event => {
  const button = event.target.closest('button[data-action]');
  if (!button || button.closest('#workspace') || !adapter) return;
  const action = button.dataset.action;
  adapter.record('event', { source: event.detail === 0 ? 'keyboard-click' : 'click', action, trusted: event.isTrusted });
  if (action === 'new-workspace') {
    try { createWorkspace(); adapter.status('New host workspace created from requested counts and depth.'); }
    catch { adapter.status('Requested workspace rejected by the host live budget. Current workspace retained.', 'error'); }
    return;
  }
  if (action === 'save-layout') { adapter.save(); return; }
  if (action === 'load-layout') { editorTask = undefined; adapter.load(); return; }
  if (action === 'cancel-preview') { adapter.cancelPreview('button'); return; }
  const { snapshot, frame, destination, workspace, view, task } = selected();
  if (!frame) return;
  switch (action) {
    case 'dock': case 'dock-left': case 'dock-right':
      if (destination) previewCommit({ kind: 'dock', frame: frame.id, location: { kind: 'beside', frame: destination.id, edge: action === 'dock-left' ? 'Left' : action === 'dock-right' ? 'Right' : $('#dock-edge').value } });
      break;
    case 'move-frame': if (workspace) adapter.command({ kind: 'move_frame', frame: frame.id, destination: workspace.id, index: 0 }); break;
    case 'move-view': if (view && destination) adapter.command({ kind: 'move_view', view: view.id, destination: destination.id, index: 0 }); break;
    case 'float': {
      const owner = adapter;
      const frameId = frame.id;
      owner.queueMeasure();
      owner.whenIdle().then(() => {
        if (adapter !== owner || owner.disposed) return;
        const current = owner.snapshot();
        const target = list(current?.frames).find(item => item.id === frameId);
        const viewport = list(current?.workspaces).find(item => item.id === target?.workspace)?.rect;
        if (!target?.rect || !viewport) return;
        const rect = { ...target.rect,
          x: Math.max(viewport.x, Math.min(target.rect.x + 24, viewport.x + viewport.width - target.rect.width)),
          y: Math.max(viewport.y, Math.min(target.rect.y + 24, viewport.y + viewport.height - target.rect.height)) };
        const result = owner.preview({ kind: 'float', frame: frameId, rect });
        if (result.ok) owner.commitPreview();
      }).catch(() => owner.status('Float request failed. Current workspace retained.', 'error'));
      break;
    }
    case 'redock': adapter.command({ kind: 'redock', frame: frame.id }); break;
    case 'expand-selected': adapter.command({ kind: 'expand', frame: frame.id, expanded: !frame.expanded }); break;
    case 'maximize': adapter.command({ kind: 'maximize', frame: frame.id }); break;
    case 'restore-layout': adapter.command({ kind: 'restore_layout', workspace: frame.workspace }); break;
    case 'equalize': adapter.command({ kind: 'equalize', workspace: frame.workspace }); break;
    case 'swap': if (destination) adapter.command({ kind: 'swap', first: frame.id, second: destination.id }); break;
    case 'select-view': if (view) adapter.command({ kind: 'select', view: view.id }); break;
    case 'close': if (view) adapter.command({ kind: 'close', view: view.id, approved: false }); break;
    case 'save-task': if (task) adapter.hostCommand(view.id, { kind: 'edit', title: $('#task-title').value, body: $('#task-body').value }); break;
    case 'domain': if (task) adapter.hostCommand(view.id, { kind: 'domain', column: $('#task-column').value, status: $('#task-status').value }); break;
    case 'veto': if (task) adapter.hostCommand(view.id, { kind: 'veto', enabled: !task.veto }); break;
    case 'provider-toggle': {
      const provider = list(snapshot.providers).find(item => item.id === view?.provider);
      if (provider) adapter.command({ kind: 'provider_availability', provider: provider.id, available: !provider.available });
      break;
    }
    case 'grid-tracks': {
      const grid = list(snapshot.nodes).find(node => node.kind === 'grid' && node.workspace === frame.workspace);
      if (grid) previewCommit({ kind: 'grid_tracks', workspace: grid.workspace, node: grid.id, rows: grid.rows.map(() => 1), columns: grid.columns.map(() => 1) });
      else adapter.status('Selected workspace has no grid tracks. Choose a nested detail workspace.', 'error');
      break;
    }
  }
});
controls.frame.addEventListener('change', () => { selectedFrame = controls.frame.value; const frame = list(adapter.current.frames).find(item => item.id === selectedFrame); controls.view.value = frame?.selected_view ?? ''; editorTask = undefined; updateInspector(adapter.current, adapter.hostCurrent); });
controls.view.addEventListener('change', () => { editorTask = undefined; updateInspector(adapter.current, adapter.hostCurrent); });
$('#workspace').addEventListener('pointerdown', event => { const frame = event.target.closest('[data-frame]'); if (frame) { selectedFrame = frame.dataset.frame; controls.frame.value = selectedFrame; const projection = list(adapter.current.frames).find(item => item.id === selectedFrame); controls.view.value = projection?.selected_view ?? ''; editorTask = undefined; updateInspector(adapter.current, adapter.hostCurrent); } });
$('#workspace').addEventListener('click', event => {
  const tab = event.target.closest('[data-action=select][data-view]');
  if (!tab) return;
  const frame = tab.closest('.frame');
  selectedFrame = frame.dataset.frame; controls.frame.value = selectedFrame;
  controls.view.value = tab.dataset.view; editorTask = undefined;
  updateInspector(adapter.current, adapter.hostCurrent);
});
$('#resize-weight').addEventListener('input', event => {
  const { frame, snapshot } = selected();
  const node = list(snapshot.nodes).find(item => item.kind === 'split' && item.workspace === frame?.workspace);
  if (!node || node.weights.length < 2) return;
  adapter.record('event', { source: 'input', action: 'resize-weight', trusted: event.isTrusted });
  const weights = [...node.weights]; const pair = weights[0] + weights[1];
  weights[0] = pair * Number(event.target.value) / 100; weights[1] = pair - weights[0];
  adapter.preview({ kind: 'resize_split', workspace: node.workspace, node: node.id, weights });
});
$('#resize-weight').addEventListener('change', () => { if (adapter.previewState) adapter.commitPreview(); });
for (const id of ['#task-title', '#task-body']) $(id).addEventListener('keydown', event => {
  const { view, frame } = selected();
  if (view && frame?.content) adapter.input({ type: event.type, isTrusted: event.isTrusted, clientX: frame.content.x + 1, clientY: frame.content.y + 1 }, view.id, `key:${event.key}`);
});
const iframe = $('#restricted-popup-frame');
function armPopupProbe() {
  const button = iframe.contentDocument?.querySelector('#request-popup');
  if (!button || button.dataset.armed) return;
  button.dataset.armed = 'true';
  for (const event of ['pointerdown', 'keydown']) button?.addEventListener(event, () => { if (adapter) popupBefore = adapter.snapshot(); });
}
iframe.addEventListener('load', armPopupProbe);
armPopupProbe();
window.addEventListener('message', event => {
  if (event.origin !== location.origin || event.source !== iframe.contentWindow || event.data?.type !== 'kumihimo-restricted-popup' || !adapter) return;
  const after = adapter.snapshot();
  window.kumihimo.popup = { ...event.data, result: event.data.outcome, before: popupBefore ?? null, after, sandbox: iframe.getAttribute('sandbox') };
  adapter.record('event', { source: 'restricted-iframe-click', action: 'popup', denied: event.data.denied, outcome: event.data.outcome, trusted: event.data.trusted });
  $('#popup-status').textContent = event.data.denied ? `Browser popup denied (${event.data.outcome}); fresh Rust source ownership read.` : 'Failure: browser unexpectedly allowed a popup; owned popup closed.';
});
try { await init(); createWorkspace(true); }
catch { $('#status').textContent = 'Rust/WASM startup failed. Build the matching generated module and serve the pilot over local HTTP.'; $('#status').dataset.state = 'error'; $('#capabilities').textContent = 'Rust/WASM unavailable · recovery required'; }
