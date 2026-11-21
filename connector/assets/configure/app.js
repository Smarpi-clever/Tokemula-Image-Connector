let statusCommand = "";
'use strict';

const byId = id => document.getElementById(id);
const keyInput = byId('api-key');
const modelsFieldset = byId('models');
const primary = byId('default-model');
const backup = byId('fallback-model');
const backupList = byId('fallbacks');
const validateButton = byId('validate');
const saveButton = byId('save');
const cancelButton = byId('cancel');
const token = location.pathname.split('/').filter(Boolean).at(-1);
let models = [];
let fallbackIds = [];
let validated = false;
let busy = false;
let finished = false;
let expiryTimer;
let expiresAtMs = 0;
let imageProfiles = [];

function showMessage(id, message, kind = '') {
  const element = byId(id);
  element.textContent = message;
  element.className = `notice ${id === 'error' ? 'error' : kind}`.trim();
  element.hidden = !message;
}

function updateControls() {
  if (!busy && !finished && expiresAtMs && Date.now() >= expiresAtMs) { finish('expired'); return; }
  keyInput.disabled = busy || finished;
  validateButton.disabled = busy || finished || !keyInput.value.trim();
  modelsFieldset.disabled = busy || finished || !validated;
  const parameterFailure = updateParameterError();
  saveButton.disabled = busy || finished || !validated || !primary.value || !!parameterFailure;
  byId('clear-incompatible').disabled = busy || finished || !validated;
  cancelButton.disabled = busy || finished;
  byId('add-fallback').disabled = busy || !validated || !backup.value;
}

function resetValidation() {
  validated = false;
  models = [];
  fallbackIds = [];
  primary.replaceChildren(new Option('验证后选择模型', ''));
  backup.replaceChildren(new Option('选择备用模型', ''));
  backupList.replaceChildren();
  byId('empty-fallbacks').hidden = false;
  byId('key-state').textContent = '仅验证权限和模型，不生成图片。';
  byId('auto-fallback').checked = false;
  byId('image-size').replaceChildren(new Option('自动／模型默认', ''));
  byId('image-quality').replaceChildren(new Option('自动／模型默认', ''));
  byId('custom-size').hidden = true;
  byId('image-complexity').textContent = '';
  byId('capability-state').textContent = '';
  updateControls();
}

async function request(action, body) {
  const headers = {'X-Tokemula-Session': token};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  let response;
  try {
    response = await fetch(`api/${action}`, {
      method: body === undefined ? 'GET' : 'POST',
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      credentials: 'omit', cache: 'no-store', referrerPolicy: 'no-referrer',
    });
  } catch {
    throw {message: '连接暂时中断。请重试，或回到客户端重新打开连接页。', outcomeUnknown: true};
  }
  let data;
  try { data = await response.json(); }
  catch { throw {message: '连接页服务暂时不可用，请回到客户端重新打开。', outcomeUnknown: true}; }
  if (!response.ok) {
    throw {message: data.error?.message || '操作未完成，请重试。', code: data.error?.code, status: data.status};
  }
  return data;
}

function finish(status, activeSource) {
  if (finished) return;
  finished = true;
  clearTimeout(expiryTimer);
  keyInput.value = '';
  validated = false;
  models = [];
  fallbackIds = [];
  byId('configure-form').hidden = true;
  showMessage('notice', '');
  showMessage('error', '');
  byId('intro').hidden = true;
  const section = byId('finished');
  const saved = status === 'saved';
  section.classList.toggle('neutral', !saved);
  section.querySelector('.result-mark').textContent = saved ? '✓' : '–';
  byId('finished-title').textContent = saved ? '设置已保存' : status === 'cancelled' ? '已取消连接' : status === 'unknown' ? '请核对连接状态' : '连接页已过期';
  byId('finished-message').textContent = saved
    ? activeSource === 'environment'
      ? '设置已保存。当前环境变量仍覆盖系统设置。请清除覆盖后再使用本次设置。'
      : '回到客户端，直接描述想要的图片即可。此页面可以关闭。'
    : status === 'cancelled'
      ? '原有设置保持不变。此页面可以关闭。'
      : status === 'unknown'
        ? `保存结果尚未取得。请在终端运行 ${statusCommand || '管理程序 status'} 核对，再决定是否重新配置。`
        : '原有设置保持不变。请回到客户端重新打开连接页。';
  section.hidden = false;
  section.focus({preventScroll: true});
  window.scrollTo(0, 0);
}

function armExpiry(expiresAt) {
  clearTimeout(expiryTimer);
  if (typeof expiresAt !== 'number') return;
  expiresAtMs = expiresAt * 1000;
  expiryTimer = setTimeout(() => { if (!busy) finish('expired'); }, Math.max(0, expiresAtMs - Date.now()));
}

function optionLabel(model) {
  return model.display_name === model.id ? model.id : `${model.display_name} (${model.id})`;
}

function currentCapability() {
  return models.find(model => model.id === primary.value)?.capabilities?.find(capability =>
    capability.operation === byId('image-operation').value && capability.mode === byId('image-mode').value);
}

function profileIndex() {
  return imageProfiles.findIndex(profile => profile.operation === byId('image-operation').value && profile.mode === byId('image-mode').value);
}

function sizeLabel(value) {
  if (value === 'auto') return '自动 (auto)';
  const dimensions = /^(\d+)x(\d+)$/.exec(value);
  if (!dimensions) return /^[124]K$/i.test(value) ? `${value} 分辨率等级` : value;
  const width = Number(dimensions[1]);
  const height = Number(dimensions[2]);
  let a = width, b = height;
  while (b) [a, b] = [b, a % b];
  return a ? `${width}×${height} · ${width / a}:${height / a}` : value;
}

function qualityLabel(value) {
  const names = {auto: '自动', low: '低', medium: '中', high: '高', xhigh: '超高', max: '最高', standard: '标准', hd: '高清'};
  return names[value] ? `${names[value]} (${value})` : value;
}

function customSizeError(value, rules) {
  const dimensions = /^(\d+)x(\d+)$/.exec(value);
  if (!dimensions) return '请输入完整的自定义宽度和高度。';
  const width = Number(dimensions[1]), height = Number(dimensions[2]);
  const pixels = width * height;
  if (![width, height, pixels].every(Number.isSafeInteger) || width < rules.min_edge || height < rules.min_edge || width > rules.max_edge || height > rules.max_edge) return `宽和高须在 ${rules.min_edge}–${rules.max_edge} 像素之间。`;
  if (width % rules.multiple_of || height % rules.multiple_of) return `宽和高须为 ${rules.multiple_of} 的倍数。`;
  if (pixels < rules.min_pixels || pixels > rules.max_pixels) return `总像素须在 ${rules.min_pixels.toLocaleString()}–${rules.max_pixels.toLocaleString()} 之间。`;
  if (Math.max(width, height) > Math.min(width, height) * rules.max_aspect_ratio) return `长宽比须在 1:${rules.max_aspect_ratio} 到 ${rules.max_aspect_ratio}:1 之间。`;
  return '';
}

function profileError(profile) {
  const parameters = models.find(model => model.id === primary.value)?.capabilities?.find(capability =>
    capability.operation === profile.operation && capability.mode === profile.mode)?.parameters;
  if (!parameters) return '当前模型不支持此操作和执行方式。';
  if (profile.size && !parameters.sizes?.includes(profile.size)) {
    if (!parameters.size_rules) return '当前模型不支持已选尺寸，请重新选择。';
    const error = customSizeError(profile.size, parameters.size_rules);
    if (error) return error;
  }
  if (profile.quality && !parameters.qualities?.includes(profile.quality)) return '当前模型不支持已选画质，请重新选择。';
  return '';
}

function incompatibleProfiles() {
  if (!validated || !primary.value) return [];
  return imageProfiles.map(profile => ({profile, error: profileError(profile)})).filter(item => item.error);
}

function incompatibilityLabel(item) {
  const operations = {generation: '生成图片', edit: '编辑图片'};
  const modes = {sync: '同步', single_async: '单任务异步', batch: '批量任务'};
  return `${operations[item.profile.operation]} · ${modes[item.profile.mode]}：${item.error}`;
}

function parameterError() {
  return incompatibleProfiles().map(incompatibilityLabel).join('\n');
}

function updateParameterError() {
  const error = parameterError();
  byId('parameter-error').textContent = error;
  byId('parameter-error').hidden = !error;
  byId('clear-incompatible').hidden = !error;
  return error;
}

function clearIncompatibleProfiles() {
  if (busy || finished) return;
  const incompatible = incompatibleProfiles();
  if (!incompatible.length || !window.confirm(`清除以下不兼容偏好？\n\n${incompatible.map(incompatibilityLabel).join('\n')}\n\n仅修改当前页面；点击“保存设置”后生效。`)) return;
  const removed = new Set(incompatible.map(item => item.profile));
  imageProfiles = imageProfiles.filter(profile => !removed.has(profile));
  renderImagePreferences();
}

function updateImageDraft() {
  const sizeSelect = byId('image-size');
  const size = sizeSelect.value === '__custom' ? `${byId('image-width').value}x${byId('image-height').value}` : sizeSelect.value;
  const quality = byId('image-quality').value;
  const index = profileIndex();
  if (index >= 0) imageProfiles.splice(index, 1);
  if (size || quality) imageProfiles.push({operation: byId('image-operation').value, mode: byId('image-mode').value, size: size || null, quality: quality || null});
  byId('custom-size').hidden = sizeSelect.value !== '__custom';
  updateControls();
}

function renderImagePreferences() {
  const parameters = currentCapability()?.parameters;
  const profile = imageProfiles[profileIndex()] || {};
  const size = byId('image-size'), quality = byId('image-quality');
  size.replaceChildren(new Option('自动／模型默认', ''));
  quality.replaceChildren(new Option('自动／模型默认', ''));
  for (const value of parameters?.sizes || []) size.add(new Option(sizeLabel(value), value));
  for (const value of parameters?.qualities || []) quality.add(new Option(qualityLabel(value), value));
  if (parameters?.size_rules) size.add(new Option('自定义宽高', '__custom'));
  const custom = profile.size && !parameters?.sizes?.includes(profile.size) && parameters?.size_rules && /^(\d*)x(\d*)$/.exec(profile.size);
  if (custom) {
    size.value = '__custom';
    byId('image-width').value = custom[1];
    byId('image-height').value = custom[2];
  } else if (profile.size) {
    if (!parameters?.sizes?.includes(profile.size)) size.add(new Option(`${sizeLabel(profile.size)}（当前不支持）`, profile.size));
    size.value = profile.size;
  }
  if (profile.quality) {
    if (!parameters?.qualities?.includes(profile.quality)) quality.add(new Option(`${qualityLabel(profile.quality)}（当前不支持）`, profile.quality));
    quality.value = profile.quality;
  }
  const rules = parameters?.size_rules;
  if (rules) {
    for (const id of ['image-width', 'image-height']) {
      byId(id).step = rules.multiple_of;
      byId(id).min = rules.min_edge;
      byId(id).max = rules.max_edge;
    }
    byId('size-rules').textContent = `宽高为 ${rules.multiple_of} 的倍数，边长 ${rules.min_edge}–${rules.max_edge} 像素；长宽比 1:${rules.max_aspect_ratio} 至 ${rules.max_aspect_ratio}:1，总像素 ${rules.min_pixels.toLocaleString()}–${rules.max_pixels.toLocaleString()}。`;
  }
  byId('custom-size').hidden = size.value !== '__custom';
  byId('capability-state').textContent = parameters ? '单次请求可覆盖这些偏好。' : '当前模型不支持此操作和执行方式。';
  size.disabled = !parameters && !profile.size;
  quality.disabled = (!parameters || !parameters.qualities?.length) && !profile.quality;
  byId('image-complexity').textContent = parameters?.complexities?.includes('medium')
    ? '图片思考复杂度：中等 (medium)。可在单次请求中指定 low / medium / high。'
    : '图片思考复杂度：此路径不支持。';
  updateControls();
}

function renderBackups(focusId) {
  backup.replaceChildren(new Option('选择备用模型', ''));
  for (const model of models) {
    if (model.id !== primary.value && !fallbackIds.includes(model.id)) backup.add(new Option(optionLabel(model), model.id));
  }
  backupList.replaceChildren();
  fallbackIds.forEach((id, index) => {
    const model = models.find(item => item.id === id);
    const row = document.createElement('li');
    const info = document.createElement('span');
    info.className = 'model-info';
    const name = document.createElement('strong');
    name.textContent = model.display_name;
    const actualId = document.createElement('span');
    actualId.className = 'model-id';
    actualId.textContent = id;
    info.append(name, actualId);
    const controls = document.createElement('span');
    controls.className = 'order-controls';
    for (const [action, text, label] of [['up', '↑', '上移'], ['down', '↓', '下移'], ['remove', '移除', '移除']]) {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = text;
      button.id = `backup-${action}-${index}`;
      button.setAttribute('aria-label', `${label} ${model.display_name}`);
      button.disabled = action === 'up' && index === 0 || action === 'down' && index === fallbackIds.length - 1;
      button.addEventListener('click', () => {
        if (action === 'remove') fallbackIds.splice(index, 1);
        else {
          const other = action === 'up' ? index - 1 : index + 1;
          [fallbackIds[index], fallbackIds[other]] = [fallbackIds[other], fallbackIds[index]];
        }
        renderBackups(action === 'remove' ? 'fallback-model' : `backup-${action}-${action === 'up' ? index - 1 : index + 1}`);
      });
      controls.append(button);
    }
    row.append(info, controls);
    backupList.append(row);
  });
  byId('empty-fallbacks').hidden = fallbackIds.length > 0;
  updateControls();
  if (focusId) byId(focusId)?.focus();
}

function applyState(state) {
  statusCommand = state.status_command || statusCommand;
  if (['saved', 'cancelled', 'expired'].includes(state.status)) { finish(state.status, state.active_source); return; }
  armExpiry(state.expires_at);
  if (state.image_preferences) imageProfiles = state.image_preferences.profiles.map(profile => ({...profile}));
  if (!state.validation) {
    resetValidation();
    if (state.validation_error) showMessage('error', state.validation_error.message);
    return;
  }
  models = state.validation.models;
  validated = models.length > 0;
  primary.replaceChildren(new Option('请选择默认模型', ''));
  for (const model of models) primary.add(new Option(optionLabel(model), model.id));
  const preferences = state.preferences;
  const ids = new Set(models.map(model => model.id));
  if (preferences && ids.has(preferences.default_model)) primary.value = preferences.default_model;
  fallbackIds = preferences ? preferences.fallback_models.filter(id => ids.has(id) && id !== primary.value) : [];
  byId('auto-fallback').checked = preferences?.auto_fallback ?? false;
  byId('key-state').textContent = 'Key 已验证，可直接修改设置；更换 Key 时重新验证。';
  keyInput.placeholder = '更换 Key（可留空）';
  if (state.validation.status === 'no_image_access') {
    showMessage('notice', '此 Key 没有图片权限。请更换 Key，或联系管理员开通。');
  } else if (!models.length) {
    showMessage('notice', '当前目录暂无图片模型。请稍后重试，或更换 Key。');
  } else if (!state.validation.can_submit) {
    const reasons = {
      insufficient_balance: '当前余额不足', quota_exhausted: '当前额度已用尽',
      subscription_unavailable: '当前订阅不可用', subscription_limit_exceeded: '当前订阅额度已用尽',
    };
    showMessage('notice', `Key 已验证，${reasons[state.validation.reason_code] || '当前无法新提交图片'}。仍可保存连接和模型偏好。`);
  } else showMessage('notice', 'Key 已验证，请确认默认模型和备用顺序。', 'success');
  renderBackups();
  renderImagePreferences();
}

function handleError(error) {
  if (error.status === 'expired' || error.code === 'session_expired') { finish('expired'); return; }
  showMessage('error', error.message || '操作未完成，请重试。');
  byId('error').focus({preventScroll: true});
  window.scrollTo(0, 0);
}

keyInput.addEventListener('input', () => {
  resetValidation();
  showMessage('notice', '');
  showMessage('error', '');
});
keyInput.addEventListener('keydown', event => {
  if (event.key === 'Enter') { event.preventDefault(); if (!validateButton.disabled) validateButton.click(); }
});
primary.addEventListener('change', () => {
  fallbackIds = fallbackIds.filter(id => id !== primary.value);
  renderBackups();
  renderImagePreferences();
});
byId('clear-incompatible').addEventListener('click', clearIncompatibleProfiles);
byId('image-operation').addEventListener('change', renderImagePreferences);
byId('image-mode').addEventListener('change', renderImagePreferences);
byId('image-size').addEventListener('change', updateImageDraft);
byId('image-quality').addEventListener('change', updateImageDraft);
byId('image-width').addEventListener('input', updateImageDraft);
byId('image-height').addEventListener('input', updateImageDraft);
backup.addEventListener('change', updateControls);
byId('add-fallback').addEventListener('click', () => {
  if (!backup.value) return;
  fallbackIds.push(backup.value);
  renderBackups('fallback-model');
});
validateButton.addEventListener('click', async () => {
  if (busy || finished || !keyInput.value.trim()) return;
  const key = keyInput.value.trim();
  keyInput.value = '';
  resetValidation();
  busy = true;
  validateButton.textContent = '验证中…';
  showMessage('notice', '');
  showMessage('error', '');
  updateControls();
  try { applyState(await request('validate', {key})); if (validated) primary.focus(); }
  catch (error) { handleError(error); if (!finished) (validated ? primary : keyInput).focus(); }
  finally { busy = false; validateButton.textContent = '验证 Key'; updateControls(); }
});
byId('configure-form').addEventListener('submit', async event => {
  event.preventDefault();
  if (busy || finished || !validated || !primary.value || updateParameterError()) return;
  busy = true;
  saveButton.textContent = '保存中…';
  showMessage('error', '');
  updateControls();
  try {
    const result = await request('save', {default_model: primary.value, fallback_models: [...fallbackIds], auto_fallback: byId('auto-fallback').checked, image_preferences: {version: 1, profiles: imageProfiles}});
    if (result.status === 'saved') finish('saved', result.active_source);
    else handleError({message: '连接尚未保存，请重试。'});
  } catch (error) { if (error.outcomeUnknown) finish('unknown'); else handleError(error); }
  finally { busy = false; saveButton.textContent = '保存设置'; updateControls(); }
});
cancelButton.addEventListener('click', async () => {
  if (busy || finished) return;
  busy = true;
  keyInput.value = '';
  updateControls();
  try {
    const result = await request('cancel', {});
    if (result.status === 'cancelled') finish('cancelled');
  } catch (error) { handleError(error); }
  finally { busy = false; updateControls(); }
});

(async () => {
  resetValidation();
  busy = true;
  updateControls();
  try { applyState(await request('state')); if (!finished) (validated ? primary : keyInput).focus(); }
  catch (error) { handleError(error); }
  finally { busy = false; updateControls(); }
})();
