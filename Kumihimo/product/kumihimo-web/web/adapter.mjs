// Browser presentation only. All arrangement, identity and preview state belongs to Rust.
const element = (tag, text, attributes = {}) => {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, String(value));
  return node;
};
const rectStyle = (node, rect, origin) => {
  node.style.left = `${rect.x - origin.x}px`;
  node.style.top = `${rect.y - origin.y}px`;
  node.style.width = `${Math.max(0, rect.width)}px`;
  node.style.height = `${Math.max(0, rect.height)}px`;
};
const entries = value => Array.isArray(value) ? value : Object.values(value ?? {});
export class BrowserAdapter {
  constructor({ host, root, storage = {}, status, onSnapshot = () => {} }) {
    if (!host?.route_json || !root) throw new TypeError('Rust host and workspace root required');
    this.host = host;
    this.root = root;
    this.statusNode = status;
    this.onSnapshot = onSnapshot;
    this.events = [];
    this.storage = {
      layoutKey: storage.layoutKey ?? 'kumihimo-pilot-layout-v1',
      hostKey: storage.hostKey ?? 'kumihimo-pilot-host-v1',
      getStore: storage.getStore ?? (() => window.localStorage),
      enabled: storage.enabled !== false,
      last: null,
    };
    this.frames = new Map();
    this.handles = new Map();
    this.previewState = null;
    this.drag = null;
    this.renderTicket = null;
    this.measureTicket = null;
    this.lastViewport = null;
    this.disposed = false;
    this.abort = new AbortController();
    const options = { signal: this.abort.signal };
    root.addEventListener('pointerdown', event => this.pointerDown(event), options);
    root.addEventListener('pointermove', event => this.pointerMove(event), options);
    root.addEventListener('pointerup', event => this.pointerUp(event), options);
    root.addEventListener('pointercancel', () => this.cancelPreview('pointercancel'), options);
    root.addEventListener('lostpointercapture', () => { if (this.drag) this.cancelPreview('lostpointercapture'); }, options);
    root.addEventListener('click', event => this.click(event), options);
    root.addEventListener('keydown', event => this.keyDown(event), options);
    window.addEventListener('blur', () => this.cancelPreview('blur'), options);
    window.addEventListener('keydown', event => {
      if (event.key === 'Escape' && this.previewState) { event.preventDefault(); this.cancelPreview('Escape'); }
    }, options);
    window.addEventListener('resize', () => this.queueMeasure(), options);
    window.addEventListener('scroll', () => this.queueMeasure(), { ...options, passive: true, capture: true });
    this.observer = new ResizeObserver(() => this.queueMeasure());
    this.observer.observe(root);
    this.refresh();
    this.queueMeasure();
  }
  record(type, detail = {}) {
    this.eventSequence = (this.eventSequence ?? 0) + 1;
    this.events.push({ sequence: this.eventSequence, type, ...detail });
    if (this.events.length > 256) this.events.splice(0, this.events.length - 256);
  }
  status(message, state = 'ready') {
    if (this.statusNode) { this.statusNode.textContent = message; this.statusNode.dataset.state = state; }
  }
  route(request) {
    let response;
    try { response = JSON.parse(this.host.route_json(JSON.stringify(request))); }
    catch { response = { version: 1, route: request.route, ok: false, error: 'boundary_failure' }; }
    this.record('route', { route: request.route, action: request.action?.kind, ok: response.ok === true });
    if (!response.ok) this.status(`Request rejected (${typeof response.error === 'string' && /^[a-zA-Z0-9_ -]{1,80}$/.test(response.error) ? response.error : 'redacted error'}). Valid state retained.`, 'error');
    return response;
  }
  snapshot() { return this.route({ version: 1, route: 'snapshot' }).result; }
  hostSnapshot() { return this.route({ version: 1, route: 'host_snapshot' }).result; }
  refresh() {
    const snapshot = this.snapshot();
    const host = this.hostSnapshot();
    if (snapshot && host) { this.current = snapshot; this.hostCurrent = host; this.queueRender(); }
    return this.current;
  }
  command(action) {
    const snapshot = this.snapshot();
    if (!snapshot) return { ok: false, error: 'snapshot_unavailable' };
    const request = { version: 1, route: 'command', identity: snapshot.identity, actor: 'browser',
      operation: snapshot.next_operation, expected_generation: snapshot.generation, action };
    const response = this.route(request);
    this.lastCommand = { request, response };
    this.refresh();
    if (response.ok) this.status(`Arrangement ${action.kind} accepted.`);
    return response;
  }
  hostCommand(view, action) {
    const snapshot = this.snapshot();
    const host = this.hostSnapshot();
    const target = entries(snapshot?.views).find(item => item.id === view);
    if (!target || !host) return { ok: false, error: 'target_unavailable' };
    const response = this.route({ version: 1, route: 'host_command', identity: snapshot.identity,
      actor: 'browser', expected_revision: host.revision, view, resource: target.resource, provider: target.provider, action });
    this.refresh();
    if (response.ok) this.status(`Host task ${action.kind} accepted.`);
    return response;
  }
  preview(action) {
    const snapshot = this.snapshot();
    if (!snapshot) return { ok: false };
    if (this.previewState) this.route({ version: 1, route: 'cancel_preview', token: this.previewState.token });
    const response = this.route({ version: 1, route: 'preview', identity: snapshot.identity,
      expected_generation: snapshot.generation, action });
    this.previewState = response.ok ? response.result : null;
    this.queueRender();
    return response;
  }
  commitPreview() {
    const token = this.previewState?.token;
    if (!token) return { ok: false, error: 'no_preview' };
    const response = this.command({ kind: 'commit_preview', token });
    this.previewState = null;
    this.queueRender();
    return response;
  }
  cancelPreview(reason = 'cancel') {
    if (this.previewState) this.route({ version: 1, route: 'cancel_preview', token: this.previewState.token });
    this.previewState = null;
    this.drag = null;
    this.record('cancel', { source: reason });
    this.queueRender();
  }
  save() {
    if (!this.storage.enabled) { this.status('Storage disabled by host policy.', 'error'); return { ok: false, error: 'storage_disabled' }; }
    const response = this.route({ version: 1, route: 'save' });
    if (!response.ok) return response;
    try {
      const store = this.storage.getStore();
      store.setItem(this.storage.layoutKey, response.result.layout);
      store.setItem(this.storage.hostKey, response.result.host);
      const layout = store.getItem(this.storage.layoutKey);
      const host = store.getItem(this.storage.hostKey);
      const ok = layout === response.result.layout && host === response.result.host;
      this.storage.last = { ok, layout, host, layoutKey: this.storage.layoutKey, hostKey: this.storage.hostKey };
      this.record('storage', { action: 'save', ok });
      this.status(ok ? 'Layout and separate host tasks saved; stored bytes reread.' : 'Storage verification failed; current workspace retained.', ok ? 'ready' : 'error');
      return { ok, result: this.storage.last };
    } catch (error) {
      this.storage.last = { ok: false, error: error?.name ?? 'StorageError' };
      this.record('storage', { action: 'save', ok: false, error: this.storage.last.error });
      this.status('Browser storage refused the save. Current workspace retained; retry Save.', 'error');
      return { ok: false, error: 'storage_refused' };
    }
  }
  load() {
    if (!this.storage.enabled) return { ok: false, error: 'storage_disabled' };
    try {
      const store = this.storage.getStore();
      const layout = store.getItem(this.storage.layoutKey);
      const host = store.getItem(this.storage.hostKey);
      if (layout === null && host === null) return { ok: true, result: { empty: true } };
      if (layout === null || host === null) { this.status('Saved pair is incomplete. Current workspace retained; Save creates a complete pair.', 'error'); return { ok: false, error: 'incomplete_storage' }; }
      const response = this.route({ version: 1, route: 'load', layout, host, fresh_incarnation: crypto.randomUUID() });
      this.record('storage', { action: 'load', ok: response.ok });
      this.refresh();
      if (response.ok) this.status('Saved workspace restored into a fresh Rust incarnation.');
      return response;
    } catch { this.status('Browser storage refused the read. Current workspace retained.', 'error'); return { ok: false, error: 'storage_refused' }; }
  }
  queueMeasure() {
    if (this.disposed || this.measureTicket !== null) return;
    this.measureTicket = requestAnimationFrame(() => {
      this.measureTicket = null;
      const bounds = this.root.getBoundingClientRect();
      const viewport = { rect: { x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height }, scale: devicePixelRatio || 1 };
      const key = JSON.stringify(viewport);
      if (key !== this.lastViewport) {
        this.lastViewport = key;
        this.cancelPreview('viewport');
        this.command({ kind: 'viewport', ...viewport });
      }
    });
  }
  queueRender() {
    if (this.disposed || this.renderTicket !== null) return;
    this.renderTicket = requestAnimationFrame(() => { this.renderTicket = null; this.render(); });
  }
  render() {
    if (!this.current) return;
    const origin = this.root.getBoundingClientRect();
    const views = new Map(entries(this.current.views).map(view => [view.id, view]));
    const tasks = new Map(entries(this.hostCurrent.tasks).flatMap(task => entries(task.views).map(view => [typeof view === 'string' ? view : view.id, task])));
    const present = new Set();
    for (const frame of entries(this.current.frames)) {
      present.add(frame.id);
      let node = this.frames.get(frame.id);
      if (!node) {
        node = element('section', undefined, { class: 'frame', 'data-frame': frame.id, role: 'group', tabindex: '-1' });
        const header = element('div', undefined, { class: 'frame-header', 'data-action': 'drag-frame', 'data-frame': frame.id, tabindex: '0', role: 'toolbar' });
        header.append(element('span', '', { class: 'frame-title' }), element('div', undefined, { class: 'tabs', role: 'tablist', 'aria-label': `Frame ${frame.id} tabs` }),
          element('button', '−', { 'data-action': 'expand', 'data-frame': frame.id }));
        node.append(header, element('div', '', { class: 'frame-content', 'data-action': 'content', 'data-frame': frame.id }));
        this.root.append(node); this.frames.set(frame.id, node);
      }
      node.hidden = !frame.visible;
      node.dataset.workspace = frame.workspace;
      node.dataset.expanded = String(frame.expanded);
      node.style.zIndex = String(10 + Number(frame.depth ?? 0));
      rectStyle(node, frame.rect, origin);
      const clip = frame.clip ?? frame.rect;
      node.style.clipPath = `inset(${Math.max(0, clip.y - frame.rect.y)}px ${Math.max(0, frame.rect.x + frame.rect.width - clip.x - clip.width)}px ${Math.max(0, frame.rect.y + frame.rect.height - clip.y - clip.height)}px ${Math.max(0, clip.x - frame.rect.x)}px)`;
      const task = tasks.get(frame.selected_view);
      node.setAttribute('aria-label', task ? `${task.title} · Portal-style demo card` : `Detail frame ${frame.id}`);
      node.querySelector('.frame-title').textContent = task?.title ?? `Detail ${frame.id}`;
      const expand = node.querySelector('[data-action=expand]');
      expand.textContent = frame.expanded ? '−' : '+';
      expand.setAttribute('aria-expanded', String(frame.expanded));
      expand.setAttribute('aria-label', `${frame.expanded ? 'Collapse' : 'Expand'} frame ${frame.id}`);
      const tabs = node.querySelector('.tabs');
      const tabNodes = new Map([...tabs.children].map(tab => [tab.dataset.view, tab]));
      const wanted = new Set();
      for (const viewId of entries(frame.views)) {
        const id = typeof viewId === 'string' ? viewId : viewId.id;
        wanted.add(id);
        let tab = tabNodes.get(id);
        if (!tab) { tab = element('button', '', { role: 'tab', 'data-action': 'select', 'data-view': id }); tabs.append(tab); }
        const group = entries(this.current.groups).find(item => item.id === views.get(id)?.group);
        tab.textContent = `${group?.label ?? group?.name ? `${group.label ?? group.name} · ` : ''}${tasks.get(id)?.title ?? views.get(id)?.label ?? views.get(id)?.title ?? `View ${id}`}`;
        tab.setAttribute('aria-selected', String(id === frame.selected_view));
        tab.tabIndex = id === frame.selected_view ? 0 : -1;
        if (group) { tab.dataset.group = group.id; tab.setAttribute('aria-label', `${tab.textContent}, group ${group.label ?? group.name ?? group.id}`); }
      }
      for (const [id, tab] of tabNodes) if (!wanted.has(id)) tab.remove();
      const content = node.querySelector('.frame-content');
      const view = views.get(frame.selected_view);
      const provider = entries(this.current.providers).find(item => item.id === view?.provider);
      if (view?.placeholder) content.dataset.placeholder = 'provider-unavailable';
      else delete content.dataset.placeholder;
      content.dataset.view = frame.selected_view ?? '';
      content.textContent = provider?.available === false ? 'Provider unavailable. Select this frame and use Restore provider.' : frame.child_workspace && frame.expanded ? '' : task ? `${task.column} · ${task.status}\n${task.body}` : `Host content · view ${frame.selected_view ?? 'none'}`;
      if (frame.content) {
        rectStyle(content, frame.content, frame.rect);
      }
      content.hidden = !frame.expanded;
    }
    for (const [id, node] of this.frames) if (!present.has(id)) { node.remove(); this.frames.delete(id); }
    this.renderHandles(origin);
    this.root.querySelectorAll('.preview-frame').forEach(node => node.remove());
    for (const frame of entries(this.previewState?.geometry?.frames ?? this.previewState?.geometry)) {
      if (!frame.visible || !frame.rect) continue;
      const node = element('div', undefined, { class: 'preview-frame', 'data-preview-frame': frame.id, 'aria-hidden': 'true' });
      rectStyle(node, frame.rect, origin); this.root.append(node);
    }
    this.onSnapshot(this.current, this.hostCurrent);
  }
  renderHandles(origin) {
    const wanted = new Set();
    for (const node of entries(this.current.nodes)) {
      if (node.kind !== 'split' || !node.rect || !node.weights?.length || node.visible === false) continue;
      let sum = 0;
      const total = node.weights.reduce((value, weight) => value + weight, 0);
      for (let i = 0; i < node.weights.length - 1; i++) {
        sum += node.weights[i];
        const canonicalHandle = entries(this.current.split_handles).find(handle => handle.workspace === node.workspace && handle.node === node.id && handle.first === node.children[i]);
        if (!canonicalHandle?.visible || !canonicalHandle.rect) continue;
        const key = `${node.workspace}:${node.id}:${i}`; wanted.add(key);
        let handle = this.handles.get(key);
        if (!handle) { handle = element('div', undefined, { class: 'split-handle', role: 'separator', tabindex: '0', 'data-action': 'resize-split', 'data-workspace': node.workspace, 'data-node': node.id, 'data-divider': i, 'aria-label': `Resize split ${node.id}` }); this.root.append(handle); this.handles.set(key, handle); }
        const horizontal = node.axis === 'Horizontal' || node.axis === 'horizontal';
        const fraction = sum / total;
        rectStyle(handle, canonicalHandle.rect, origin);
        handle.dataset.axis = horizontal ? 'horizontal' : 'vertical';
        handle.setAttribute('aria-orientation', horizontal ? 'vertical' : 'horizontal');
        handle.setAttribute('aria-valuemin', '1'); handle.setAttribute('aria-valuemax', '99'); handle.setAttribute('aria-valuenow', String(Math.round(fraction * 100)));
      }
    }
    for (const [key, node] of this.handles) if (!wanted.has(key)) { node.remove(); this.handles.delete(key); }
  }
  click(event) {
    const button = event.target.closest('[data-action]');
    if (!button || !this.root.contains(button)) return;
    if (button.dataset.action === 'select') { this.record('event', { source: 'click', action: 'select', trusted: event.isTrusted }); this.command({ kind: 'select', view: button.dataset.view }); }
    if (button.dataset.action === 'expand') { const frame = entries(this.current.frames).find(item => item.id === button.dataset.frame); this.record('event', { source: 'click', action: 'expand', trusted: event.isTrusted }); this.command({ kind: 'expand', frame: frame.id, expanded: !frame.expanded }); }
    if (button.dataset.action === 'content') this.input(event, button.dataset.view, 'pointer');
  }
  input(event, viewId, payload) {
    const view = entries(this.current.views).find(item => item.id === viewId);
    if (!view) return;
    const frame = entries(this.current.frames).find(item => item.selected_view === viewId);
    this.record('event', { source: event.type, action: 'input', view: viewId, trusted: event.isTrusted });
    return this.route({ version: 1, route: 'input', identity: this.current.identity, epoch: this.current.epoch,
      view: viewId, resource: view.resource, provider: view.provider, x: event.clientX ?? frame.content.x + 1,
      y: event.clientY ?? frame.content.y + 1, scale: this.current.scale, payload });
  }
  pointerDown(event) {
    const target = event.target.closest('[data-action]');
    if (!target || event.button !== 0 || event.target.closest('button,input,textarea,select')) return;
    const kind = target.dataset.action;
    if (!['drag-frame', 'resize-split'].includes(kind)) return;
    this.record('event', { source: 'pointerdown', action: kind, trusted: event.isTrusted });
    this.drag = { kind, pointerId: event.pointerId, frame: target.dataset.frame, workspace: target.dataset.workspace,
      node: target.dataset.node, divider: Number(target.dataset.divider), x: event.clientX, y: event.clientY,
      basis: entries(this.current.nodes).find(node => node.id === target.dataset.node && node.workspace === target.dataset.workspace) };
    this.root.setPointerCapture(event.pointerId); event.preventDefault();
  }
  resizeAction(node, divider, delta) {
    const horizontal = node.axis === 'Horizontal' || node.axis === 'horizontal';
    const length = horizontal ? node.rect.width : node.rect.height;
    const weights = [...node.weights];
    const total = weights.reduce((sum, weight) => sum + weight, 0);
    const change = delta / Math.max(1, length) * total;
    const pair = weights[divider] + weights[divider + 1];
    weights[divider] = Math.max(total * 0.01, Math.min(pair - total * 0.01, weights[divider] + change));
    weights[divider + 1] = pair - weights[divider];
    return { kind: 'resize_split', workspace: node.workspace, node: node.id, weights };
  }
  pointerMove(event) {
    const drag = this.drag;
    if (!drag || drag.pointerId !== event.pointerId) return;
    if (Math.hypot(event.clientX - drag.x, event.clientY - drag.y) < 4) return;
    this.record('event', { source: 'pointermove', action: drag.kind, trusted: event.isTrusted });
    if (drag.kind === 'resize-split' && drag.basis) {
      const horizontal = drag.basis.axis === 'Horizontal' || drag.basis.axis === 'horizontal';
      this.preview(this.resizeAction(drag.basis, drag.divider, horizontal ? event.clientX - drag.x : event.clientY - drag.y));
    } else {
      const target = entries(this.current.frames).filter(frame => frame.visible && frame.id !== drag.frame && event.clientX >= frame.rect.x && event.clientX <= frame.rect.x + frame.rect.width && event.clientY >= frame.rect.y && event.clientY <= frame.rect.y + frame.rect.height).sort((a, b) => b.depth - a.depth)[0];
      if (!target) return;
      const distances = [['Left', event.clientX - target.rect.x], ['Right', target.rect.x + target.rect.width - event.clientX], ['Top', event.clientY - target.rect.y], ['Bottom', target.rect.y + target.rect.height - event.clientY]];
      const edge = distances.sort((a, b) => a[1] - b[1])[0][0];
      this.preview({ kind: 'dock', frame: drag.frame, location: { kind: 'beside', frame: target.id, edge } });
    }
  }
  pointerUp(event) {
    if (!this.drag || this.drag.pointerId !== event.pointerId) return;
    this.record('event', { source: 'pointerup', action: this.drag.kind, trusted: event.isTrusted });
    this.drag = null;
    if (this.previewState) this.commitPreview();
    if (this.root.hasPointerCapture(event.pointerId)) this.root.releasePointerCapture(event.pointerId);
  }
  keyDown(event) {
    const target = event.target.closest('[data-action]');
    if (!target || event.target.closest('input,textarea,select,[contenteditable]')) return;
    if (target.dataset.action === 'select' && ['ArrowLeft', 'ArrowRight'].includes(event.key)) {
      const tabs = [...target.parentElement.children]; const next = tabs[(tabs.indexOf(target) + (event.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length];
      event.preventDefault(); this.record('event', { source: 'keydown', action: 'select', key: event.key, trusted: event.isTrusted }); this.command({ kind: 'select', view: next.dataset.view }); next.focus();
    }
    if (target.dataset.action === 'resize-split' && ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) {
      const node = entries(this.current.nodes).find(item => item.id === target.dataset.node && item.workspace === target.dataset.workspace);
      if (node) { event.preventDefault(); this.record('event', { source: 'keydown', action: 'resize-split', key: event.key, trusted: event.isTrusted }); this.preview(this.resizeAction(node, Number(target.dataset.divider), ['ArrowLeft', 'ArrowUp'].includes(event.key) ? -12 : 12)); this.commitPreview(); }
    }
  }
  async whenIdle() { await new Promise(resolve => requestAnimationFrame(resolve)); while (this.renderTicket !== null || this.measureTicket !== null) await new Promise(resolve => requestAnimationFrame(resolve)); return this.current; }
  dispose() { this.disposed = true; this.cancelPreview('dispose'); this.observer.disconnect(); this.abort.abort(); if (this.renderTicket !== null) cancelAnimationFrame(this.renderTicket); if (this.measureTicket !== null) cancelAnimationFrame(this.measureTicket); }
}
