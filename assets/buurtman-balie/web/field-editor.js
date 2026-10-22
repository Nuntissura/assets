import { createLocalization, bindLocalization } from './localization.js';
/** Reusable canonical catalogue selector and label-first creation form. */
export function mountFieldEditor(root, adapter, { onChange = () => {}, localization = createLocalization() } = {}) {
  const NEW = '@create-new';
  let disposed = false, pending, saving = false, loaded = false, refreshVersion = 0;
  let fields = [], statusKey, statusValues = {};
  const t = (key, values) => localization.text(key, values);
  const element = (tag, text) => { const el = document.createElement(tag); if (text) el.textContent = text; return el; };
  const panel = element('section'); panel.className = 'crm-panel';
  panel.append(element('h2', 'Reusable fields'), element('p', 'Define a field once. Use it on products and in search filters.'));
  const form = element('form'); form.dataset.testid = 'field-editor';
  const selectorLabel = element('label', 'Choose a field');
  const selector = element('select'); selector.name = 'existing'; selector.dataset.testid = 'field-selector'; selector.setAttribute('aria-label', 'Choose a field');
  selector.append(new Option('Create new field', NEW)); selectorLabel.append(selector); form.append(selectorLabel);
  const selected = element('p'); selected.dataset.testid = 'field-definition'; selected.style.overflowWrap = 'anywhere'; form.append(selected);
  const draft = element('fieldset'); draft.append(element('legend', 'Create new field'));
  const label = element('label', 'Field name'); const name = element('input'); name.name = 'label'; name.required = true; label.append(name); draft.append(label);
  const preview = element('p'); preview.dataset.testid = 'field-key-preview'; preview.style.overflowWrap = 'anywhere';
  const advanced = element('details'); advanced.dataset.testid = 'field-advanced'; advanced.append(element('summary', 'Advanced: technical key'), preview);
  const overrideLabel = element('label', 'Use a custom key'); const override = element('input'); override.type = 'checkbox'; override.name = 'keyOverride'; overrideLabel.prepend(override); advanced.append(overrideLabel);
  const keyLabel = element('label', 'Field key'); const key = element('input'); key.name = 'key'; key.maxLength = 128; key.pattern = '[A-Za-z0-9_.:-]+'; key.readOnly = true; key.required = true; keyLabel.append(key); advanced.append(keyLabel); draft.append(advanced);
  const typeLabel = element('label', 'Field type'); const type = element('select'); type.name = 'type'; type.setAttribute('aria-label', 'Field type');
  for (const value of ['Text', 'Integer', 'Boolean']) type.append(new Option(value, value));
  typeLabel.append(type); draft.append(typeLabel);
  const applies = element('fieldset'); applies.append(element('legend', 'Available on'));
  for (const kind of ['Product', 'Customer', 'Distributor', 'Organization', 'SupportCase', 'Interaction']) {
    const row = element('label', kind === 'SupportCase' ? 'Support case' : kind); const input = element('input'); input.type = 'checkbox'; input.name = 'kind'; input.value = kind; input.defaultChecked = kind === 'Product'; row.prepend(input); applies.append(row);
  }
  draft.append(applies); form.append(draft);
  // A pending receipt can be reconciled independently of current form validity/mode.
  const button = element('button', 'Create field'); button.type = 'button'; form.append(button);
  const refreshButton = element('button', 'Refresh fields'); refreshButton.type = 'button'; refreshButton.dataset.testid = 'field-refresh'; form.append(refreshButton);
  const status = element('p'); status.setAttribute('role', 'status');
  const list = element('ul'); list.dataset.testid = 'field-list'; panel.append(form, status, list); root.append(panel);
  const unbind = bindLocalization(panel, localization);
  const kinds = () => [...applies.querySelectorAll('input:checked')].map(input => input.value);
  const draftSignature = () => JSON.stringify([selector.value, name.value, key.value, override.checked, type.value, kinds()]);
  const kindLabel = kind => t(kind === 'SupportCase' ? 'Support case' : kind);
  function setStatus(message, values = {}) {
    statusKey = message; statusValues = values;
    status.textContent = t(message, {...values, error: values.errorKey ? localization.error(values.errorKey) : values.error});
  }
  function generateKey() {
    const base = name.value.trim().normalize('NFKD').replace(/\p{M}/gu, '').replace(/[A-Z]/g, c => c.toLowerCase()).replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 120).replace(/-+$/g, '') || 'field';
    const occupied = new Set(fields.map(field => field.key));
    let candidate = base;
    for (let n = 2; occupied.has(candidate); n++) {
      const suffix = `-${n}`; candidate = base.slice(0, 128 - suffix.length).replace(/-+$/g, '') + suffix;
    }
    return candidate;
  }
  function renderMode() {
    const creating = selector.value === NEW;
    draft.hidden = !creating; draft.disabled = !creating;
    const field = fields.find(item => item.key === selector.value);
    selected.hidden = !field;
    selected.textContent = field ? `${field.label} · ${t(field.value_type)} · ${field.applies_to.map(kindLabel).join(', ')} · ${t('Canonical key: {key}', {key: field.key})}` : '';
    key.readOnly = !override.checked;
    preview.textContent = t('Canonical key: {key}', {key: key.value});
    button.hidden = !creating && !pending;
    button.disabled = saving || (!pending && !loaded);
    button.textContent = t(pending ? 'Retry pending field' : 'Create field');
  }
  function renderFields() {
    const previous = selector.value;
    selector.replaceChildren(new Option(t('Create new field'), NEW), ...fields.map(field => new Option(`${field.label} · ${t(field.value_type)}`, field.key)));
    selector.value = fields.some(field => field.key === previous) ? previous : NEW;
    list.replaceChildren();
    for (const field of fields) { const row = element('li', `${field.label} · ${t(field.value_type)} · ${field.applies_to.map(kindLabel).join(', ')}`); row.dataset.fieldKey = field.key; list.append(row); }
    renderMode();
  }
  function updateKey() { if (!override.checked && loaded) key.value = generateKey(); renderMode(); }
  name.addEventListener('input', updateKey); key.addEventListener('input', renderMode);
  override.addEventListener('change', updateKey); selector.addEventListener('change', renderMode);
  const unsubscribe = localization.subscribe(() => { renderFields(); if (statusKey) setStatus(statusKey, statusValues); });
  async function refresh() {
    const version = ++refreshVersion;
    try {
      const catalogue = await adapter.listFields(); if (disposed || version !== refreshVersion) return;
      fields = catalogue; loaded = true; updateKey(); renderFields(); await onChange(fields);
    } catch (error) {
      if (!disposed && version === refreshVersion) { loaded = false; renderMode(); }
      throw error;
    }
  }
  refreshButton.addEventListener('click', async () => {
    if (disposed || refreshButton.disabled) return;
    refreshButton.disabled = true;
    try {
      await refresh();
      if (!disposed && !saving && !pending) setStatus('Fields refreshed.');
    } catch (error) { if (!disposed) setStatus('Fields unavailable: {error}', {errorKey: error.message}); }
    finally { if (!disposed) refreshButton.disabled = false; }
  });
  async function submit(event) {
    event.preventDefault(); if (disposed || saving) return;
    if (!pending) {
      if (!loaded || selector.value !== NEW) return;
      updateKey();
      if (!kinds().length) { setStatus('Select at least one record type.'); return; }
      if (!name.value.trim()) { setStatus('Enter a field name.'); return; }
      if ([...name.value.trim()].length > 128) { setStatus('Field name is too long.'); return; }
      if (!/^[A-Za-z0-9_.:-]{1,128}$/.test(key.value)) { setStatus('Field key must use letters, numbers, hyphens, underscores, dots or colons.'); advanced.open = true; return; }
      if (!form.reportValidity()) return;
      if (fields.some(field => field.key === key.value)) { setStatus('A field with this key already exists. Reuse it or choose another key.'); return; }
      const definition = Object.freeze({key: key.value, label: name.value.trim(), value_type: type.value, applies_to: Object.freeze(kinds())});
      pending = Object.freeze({definition, operationId: crypto.randomUUID(), draftSignature: draftSignature()});
    }
    const request = pending;
    saving = true; renderMode(); setStatus('Saving field…');
    try {
      await adapter.defineField(request.definition, request.operationId);
      if (disposed) return;
      const unchanged = draftSignature() === request.draftSignature;
      pending = undefined;
      if (unchanged) { form.reset(); selector.value = NEW; advanced.open = false; }
      try {
        await refresh(); if (disposed) return;
        setStatus(unchanged ? 'Field created. Available for records and filters.' : 'Previous save confirmed. Save your current changes.');
      } catch (error) { if (!disposed) setStatus('Field created. Catalogue refresh failed: {error}. Refresh before creating another field.', {errorKey: error.message}); }
    } catch (error) {
      if (!disposed) {
        const code = String(error.message).match(/^(Denied|Invalid|NotFound|Conflict|OperationReused|Capacity)(?:\b|\()/)?.[1];
        if (code) pending = undefined;
        setStatus(code === 'Conflict' ? 'A field with this key already exists. Reuse it or choose another key.' : code ? 'Field rejected: {error}. Correct the draft and try again.' : 'Field not confirmed: {error}. Retry to reconcile.', {errorKey: error.message});
      }
    } finally { if (!disposed) { saving = false; renderMode(); } }
  }
  button.addEventListener('click', submit); form.addEventListener('submit', submit);
  renderMode(); refresh().catch(error => { if (!disposed) setStatus('Fields unavailable: {error}', {errorKey: error.message}); });
  return { refresh, setLanguage: localization.setLanguage, dispose() { disposed = true; unbind(); unsubscribe(); panel.remove(); } };
}
