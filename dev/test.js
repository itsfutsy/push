// Interaction smoke test, run inside the harness with ?test: /dev/?s=list&test, /dev/?s=cold-start&test,
// /dev/?s=library&test and /dev/?s=hud-uploading&test.
// Drives the real UI through clicks and key events and checks what it renders. Results go to the console
// (console.warn for the summary, console.error for failures) so dev/snap.swift prints them.
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function run(doc, mock, page, name) {
  const $ = (sel) => doc.querySelector(sel);
  const text = (sel) => $(sel)?.textContent.trim() ?? '';
  const failures = [];
  let passed = 0;
  const check = (label, ok) => { if (ok) passed++; else { failures.push(label); console.error('FAIL', label); } };
  const key = (target, init) => target.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init }));
  const until = async (fn, ms = 4000) => { for (let t = 0; t < ms; t += 50) { if (fn()) return true; await sleep(50); } return false; };
  const zone = () => $('#zone').dataset.state;
  const rows = () => [...doc.querySelectorAll('#list > li:not(.leaving)')];
  const summary = () => console.warn(`test: ${passed} passed, ${failures.length} failed`);

  if (page === 'hud') {
    if (name !== 'hud-uploading') { console.error('FAIL the hud test expects ?s=hud-uploading'); return; }
    const hud = $('#hud');
    const shows = () => hud.dataset.state;
    const closeShown = () => getComputedStyle($('#hud-close')).opacity === '1';
    const said = [];
    new MutationObserver((records) => {
      for (const r of records) for (const n of r.addedNodes) if (n.textContent) said.push(n.textContent);
    }).observe($('#announcer'), { childList: true });

    check('hud shows the upload', await until(() => shows() === 'uploading'));
    await sleep(400); // let the entrance finish
    said.length = 0;
    mock.complete(mock.fx.uploading.id); // like core: `uploads`, then `hud` for the same upload
    check('hud says Link copied', await until(() => text('#hud-title') === 'Link copied'));
    check('hud shows the short url', text('#hud-sub').startsWith('share.example.com/i/'));
    check('finishing doesn’t blink the whole hud', hud.getAnimations().length === 0);
    await sleep(150);
    check('finishing is announced once', said.length === 1 && said[0].startsWith('Link copied'));
    check('success has no close button until hovered', !closeShown());

    // WebKit gets no hover events in the never-key HUD, so it asks core where the pointer is.
    mock.hooks.hovered = () => true;
    await sleep(3300);
    check('hover pauses auto-dismiss', shows() === 'done');
    mock.hooks.hovered = () => false;
    check('auto-dismisses once the pointer leaves', await until(() => shows() === '', 2500));

    // Clicking fires pointerenter, and the HUD hides with the pointer still on it (no pointerleave).
    mock.emit('hud', { kind: 'upload', upload: mock.fx.done[0] });
    await until(() => shows() === 'done');
    hud.dispatchEvent(new PointerEvent('pointerenter'));
    $('#hud-hit').click();
    await until(() => shows() === '');
    mock.emit('hud', { kind: 'upload', upload: mock.fx.done[2] });
    check('the next success still auto-dismisses', await until(() => shows() === 'done') && await until(() => shows() === '', 4500));

    // Notices that stay show their close button without hover.
    mock.emit('hud', { kind: 'upload', upload: { ...mock.fx.done[1], status: 'failed', url: '', error: { kind: 'network', message: 'Can’t reach the server.' } } });
    check('failure shows a close button', await until(closeShown));
    $('#hud-close').click();
    check('close dismisses', await until(() => shows() === ''));
    mock.emit('hud', { kind: 'permission' });
    check('permission notice shows a close button', await until(closeShown));
    summary();
    return;
  }

  if (page === 'library') {
    if (name !== 'library') { console.error('FAIL the library test expects ?s=library'); return; }
    const grid = $('#grid');
    const tiles = () => [...doc.querySelectorAll('.tile')];
    const names = () => tiles().map((t) => t.getAttribute('aria-label'));
    const heads = () => [...doc.querySelectorAll('.group-head h2')].map((h) => h.textContent);
    const selected = () => $('.tile[aria-selected="true"]')?.getAttribute('aria-label');
    const previewOpen = () => !$('#preview').hidden;
    const toast = () => $('#toast').classList.contains('on') && text('#toast-text');
    const total = mock.fx.done.length + mock.fx.more.length;

    check('every finished upload has a tile', tiles().length === total);
    check('count says how many', text('#count').startsWith(`${total} uploads · `));
    check('grouped by day, newest first', heads()[0] === 'Today' && heads()[1] === 'Yesterday');
    check('the newest upload comes first', names()[0] === mock.fx.done[0].fileName);
    check('pictures come from the previews', $('.tile.image .cover')?.src.startsWith('data:image/png'));
    check('an upload without a preview falls back to a file tile', await until(() =>
      $('.tile[aria-label="Q3 revenue.png"] .doc-name')?.textContent === 'Q3 revenue.png'));
    check('audio shows its size', text('.tile.audio .doc-meta') === '1.6 MB');

    // Filters and search
    $('[data-filter="image"]').click();
    check('Images shows only images', tiles().length > 0 && tiles().every((t) => t.classList.contains('image')));
    $('[data-filter="file"]').click();
    check('Files shows files and audio', names().includes('Voice Memo 14.m4a') && names().includes('brand-assets.zip')
      && !tiles().some((t) => t.classList.contains('image') || t.classList.contains('video')));
    $('[data-filter="video"]').click();
    check('Videos shows only videos', tiles().length === 2 && tiles().every((t) => t.classList.contains('video')));
    check('a filter with nothing selected keeps its count', text('#count').startsWith('2 uploads'));
    $('[data-filter="all"]').click();
    const search = $('#search');
    search.value = 'screenshot';
    search.dispatchEvent(new Event('input'));
    check('search matches names', tiles().length === 3 && names().every((n) => n.startsWith('Screenshot')));
    search.value = 'zebra';
    search.dispatchEvent(new Event('input'));
    check('no matches says so', !$('#empty').hidden && text('#empty-title') === 'No results for “zebra”');
    key(search, { key: 'Escape' });
    check('Escape clears the search', search.value === '' && tiles().length === total && $('#empty').hidden);

    // Selecting and keys
    grid.focus();
    key(grid, { key: 'ArrowRight' });
    check('an arrow key selects the first tile', selected() === names()[0]);
    key(grid, { key: 'ArrowRight' });
    check('right moves along', selected() === names()[1]);
    key(grid, { key: 'ArrowLeft' });
    key(grid, { key: 'ArrowDown' });
    const down = tiles().find((t) => t.getAttribute('aria-selected') === 'true');
    check('down moves to the row below', down && down.getBoundingClientRect().top > tiles()[0].getBoundingClientRect().bottom);
    key(grid, { key: 'c', metaKey: true });
    check('⌘C copies the selected link', await until(() => toast() === 'Link copied'));
    tiles()[3].querySelector('.act').click();
    check('the hover button copies too', await until(() => tiles()[3].querySelector('.act').classList.contains('done')));
    key(grid, { key: 'Escape' });
    check('Escape deselects', !selected());

    // Preview
    tiles()[0].click();
    key(grid, { key: ' ' });
    check('Space opens the preview', previewOpen() && text('#pv-name') === names()[0]);
    check('the preview shows the link', text('#pv-url') === mock.fx.done[0].url.replace('https://', ''));
    check('first item has no Previous', $('#pv-prev').disabled && !$('#pv-next').disabled);
    check('the grid is inert under it', grid.inert);
    key($('#preview'), { key: 'ArrowRight' });
    check('right shows the next upload', text('#pv-name') === names()[1] && selected() === names()[1]);
    $('#pv-link').click();
    check('Copy Link says Copied', await until(() => text('#pv-copy') === 'Copied'));
    $('#pv-delete').click();
    check('Delete asks first', !$('#pv-confirm').hidden && $('#pv-actions').hidden);
    key($('#preview'), { key: 'Escape' });
    check('Escape cancels the delete, not the preview', $('#pv-confirm').hidden && previewOpen());
    const gone = names()[1], after = names()[2];
    $('#pv-delete').click();
    $('#pv-really').click();
    check('deleting moves on to the next upload', await until(() => text('#pv-name') === after));
    check('and its tile goes', !names().includes(gone) && tiles().length === total - 1);
    key($('#preview'), { key: 'Escape' });
    check('Escape closes the preview', await until(() => !previewOpen()));
    check('the grid is back', !grid.inert && doc.activeElement === grid);

    // Selecting several, and deleting them
    const picked = () => tiles().filter((t) => t.getAttribute('aria-selected') === 'true').map((t) => t.getAttribute('aria-label'));
    const click = (i, init = {}) => tiles()[i].dispatchEvent(new MouseEvent('click', { bubbles: true, ...init }));
    const bar = () => $('#selbar').classList.contains('on');
    click(0);
    check('one selected tile shows no bar', picked().length === 1 && !bar());
    click(2, { metaKey: true });
    check('⌘-click adds a tile', picked().length === 2 && bar() && text('#sel-title') === '2 selected');
    click(2, { metaKey: true });
    check('⌘-click again takes it out', picked().length === 1 && !bar());
    click(0);
    click(3, { shiftKey: true });
    check('Shift-click selects the range', picked().join() === names().slice(0, 4).join());
    grid.focus();
    key(grid, { key: 'ArrowRight', shiftKey: true });
    check('Shift-arrow extends it', picked().join() === names().slice(0, 5).join());
    key(grid, { key: 'a', metaKey: true });
    check('⌘A selects everything shown', picked().length === tiles().length && text('#sel-all') === 'Deselect All');
    $('#sel-all').click();
    check('Deselect All clears it', !picked().length && !bar());
    click(0);
    click(1, { metaKey: true });
    const doomed = picked(), before = tiles().length;
    key(grid, { key: 'Backspace', metaKey: true });
    check('⌘⌫ asks first', text('#sel-title') === 'Delete 2 uploads?' && !$('#sel-really').hidden && $('#sel-delete').hidden);
    key(doc.activeElement, { key: 'Escape' });
    check('Escape cancels it', $('#sel-really').hidden && picked().length === 2);
    $('#sel-delete').click();
    $('#sel-really').click();
    check('the bar shows the progress', text('#sel-title').startsWith('Deleting'));
    check('the selected tiles go', await until(() => tiles().length === before - 2 && !doomed.some((n) => names().includes(n))));
    check('and it says so', await until(() => toast() === '2 uploads deleted'));
    check('the bar goes with them', !bar());
    mock.scenario.deleteFails = true;
    click(0);
    click(1, { metaKey: true });
    key(grid, { key: 'Backspace' });
    $('#sel-really').click();
    check('a failure is shown', await until(() => toast() === 'Can’t reach the server.'));
    check('and what’s left stays selected', tiles().length === before - 2 && picked().length === 2 && bar());
    mock.scenario.deleteFails = false;
    key(grid, { key: 'Escape' });

    // New uploads appear at the top
    mock.add([{ name: 'Fresh Shot.png', size: 900_000 }]);
    await until(() => mock.uploads().some((u) => u.fileName === 'Fresh Shot.png' && u.status === 'uploading'));
    check('uploads in progress aren’t shown', !names().includes('Fresh Shot.png'));
    mock.complete(mock.uploads().find((u) => u.fileName === 'Fresh Shot.png').id);
    check('a finished upload appears first', await until(() => names()[0] === 'Fresh Shot.png'));
    summary();
    return;
  }

  if (name === 'cold-start') {
    // The upload finished, and the tray asked for Settings, while the window waited for get_info.
    check('an upload that finished during startup shows as done', await until(() => rows()[0]?.dataset.mode === 'done'));
    check('drop zone isn’t stuck uploading', zone() !== 'uploading');
    check('navigate during startup shows Settings', await until(() => !$('#settings').hidden));
    summary();
    return;
  }

  if (!rows().length) { console.error('FAIL the test expects ?s=list'); return; }

  // Drag states
  mock.drag('enter', ['/a.png', '/b.png']);
  check('drag enter shows over state', await until(() => zone() === 'over'));
  check('drag copy counts files', text('#zone-sub') === 'The links are copied when they’re done');
  mock.drag('leave');
  check('drag leave returns to idle', await until(() => zone() === 'idle'));

  // Copy on row click, then revert
  const first = rows()[0];
  first.querySelector('.row-hit').click();
  check('row click shows Link copied', await until(() => first.querySelector('.meta').textContent === 'Link copied'));
  check('copied reverts', await until(() => first.querySelector('.meta').textContent.includes('·'), 2500));

  // Delete: confirm, Esc cancels, confirm again and delete
  const count = rows().length;
  rows()[1].querySelector('.del').click();
  check('delete asks to confirm', await until(() => rows()[1].dataset.mode === 'confirm'));
  check('confirm copy', rows()[1].querySelector('.name').textContent.startsWith('Delete “'));
  key(doc.body, { key: 'Escape' });
  check('Esc cancels confirm', await until(() => rows()[1].dataset.mode === 'done'));
  rows()[1].querySelector('.del').click();
  await until(() => rows()[1].dataset.mode === 'confirm');
  rows()[1].querySelector('.danger').click();
  check('delete removes the row', await until(() => rows().length === count - 1, 3000));

  // Upload through the picker: queued, progress, success, idle
  $('#zone-hit').click();
  check('picked file starts uploading', await until(() => zone() === 'uploading'));
  check('in-flight row shows progress', await until(() => /of 1\.8 MB/.test(rows()[0].querySelector('.meta').textContent)));
  check('success state', await until(() => zone() === 'success', 6000));
  check('zone returns to idle', await until(() => zone() === 'idle', 3000));

  // Cancel an in-flight upload from its row
  $('#zone-hit').click();
  await until(() => rows()[0]?.dataset.mode === 'active');
  rows()[0].querySelector('.stop').click();
  check('stop cancels the upload', await until(() => zone() === 'idle' && rows()[0].dataset.mode === 'done'));

  // A failing upload, then Try Again
  $('#zone-hit').click(); // the mock's third pick fails at 45%
  check('failure shows the error state', await until(() => zone() === 'error', 6000));
  check('network error copy', text('#zone-title') === 'Can’t reach share.example.com');
  check('failed row offers retry', !!rows()[0].querySelector('[aria-label="Try Again"]'));
  $('#zone-action').click();
  check('Try Again re-uploads', await until(() => zone() === 'uploading'));
  check('retry succeeds', await until(() => zone() === 'success', 6000));

  // A file and a folder in one drop: the folder fails at once and sorts first, the file uploads.
  mock.add([{ name: 'notes.txt', size: 1_200 }, { name: 'Folder', size: 0 }]);
  check('mixed drop reports the failed item', await until(() => zone() === 'error' && text('#zone-title') === 'Can’t upload this item', 6000));
  $('#zone-action').click();
  check('OK dismisses the failure', await until(() => zone() === 'idle'));

  // Settings: Cmd+, opens, Esc closes
  key(doc.body, { key: ',', metaKey: true });
  check('Cmd+, opens Settings', await until(() => !$('#settings').hidden && $('#main').hidden));
  check('status connects', await until(() => text('#status-text') === 'Connected'));
  key(doc.body, { key: 'Escape' });
  check('Esc closes Settings', await until(() => $('#settings').hidden));
  key(doc.body, { key: ',', metaKey: true });
  await until(() => !$('#settings').hidden);

  // Shortcut recorder
  const rec = $('#rec-key');
  rec.click();
  check('recorder records', $('#recorder').classList.contains('recording') && rec.textContent === 'Type shortcut');
  check('recorder takes focus', doc.activeElement === rec);
  key(rec, { key: 'Shift', shiftKey: true, metaKey: true });
  check('held modifiers show', rec.textContent === '⇧⌘');
  key(rec, { key: 'k', code: 'KeyK' });
  check('plain key asks for a modifier', !$('#shortcut-err').hidden);
  key(rec, { key: 'K', code: 'KeyK', shiftKey: true, metaKey: true });
  check('shortcut saved', await until(() => rec.textContent === '⇧⌘K' && !$('#recorder').classList.contains('recording')));
  // Clicking it again stops recording. WebKit blurs a focused button on mousedown, before the click.
  rec.click();
  if (rec.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }))) rec.blur();
  rec.click();
  check('clicking the recorder again stops recording', await until(() => !$('#recorder').classList.contains('recording')));
  check('recorder shows the shortcut again', rec.textContent === '⇧⌘K');
  rec.click();
  key(rec, { key: 'Escape' });
  check('Esc cancels recording', await until(() => rec.textContent === '⇧⌘K'));
  check('Esc in recorder keeps Settings open', !$('#settings').hidden);
  rec.click();
  key(rec, { key: 'Meta', code: 'MetaLeft', metaKey: true });
  check('one Command key shows as held', rec.textContent === '⌘' && $('#recorder').classList.contains('recording'));
  key(rec, { key: 'Meta', code: 'MetaRight', metaKey: true });
  check('both Command keys save', await until(() => rec.textContent === '⌘ + ⌘' && !$('#recorder').classList.contains('recording')));
  rec.click();
  key(rec, { key: 'Meta', code: 'MetaLeft', metaKey: true });
  rec.dispatchEvent(new KeyboardEvent('keyup', { bubbles: true, cancelable: true, key: 'Meta', code: 'MetaLeft' }));
  key(rec, { key: 'Meta', code: 'MetaRight', metaKey: true });
  check('Command keys one after the other don\'t save', $('#recorder').classList.contains('recording'));
  key(rec, { key: 'Shift', shiftKey: true, metaKey: true });
  key(rec, { key: 'Meta', code: 'MetaLeft', shiftKey: true, metaKey: true });
  rec.dispatchEvent(new KeyboardEvent('keyup', { bubbles: true, cancelable: true, key: 'Shift', code: 'ShiftLeft', metaKey: true }));
  key(rec, { key: 'Meta', code: 'MetaLeft', metaKey: true }); // WebKit: left Command let go, right still held
  check('letting go of a Command key doesn\'t save', $('#recorder').classList.contains('recording'));
  key(rec, { key: 'Escape' });
  await until(() => !$('#recorder').classList.contains('recording'));
  rec.click();
  key(rec, { key: '#', code: 'Digit3', shiftKey: true, metaKey: true });
  check('conflict error shown', await until(() => $('#shortcut-err').textContent === 'That shortcut is in use. Try another.'));
  check('still recording after a conflict', $('#recorder').classList.contains('recording'));
  key(rec, { key: 'Backspace' });
  check('Delete clears the shortcut', await until(() => rec.textContent === 'Record Shortcut'));
  check('clear button hidden when unset', $('#rec-clear').hidden);

  // Token and URL validation
  const token = $('#token');
  token.value = 'short';
  key(token, { key: 'Enter' });
  check('short token rejected', await until(() => $('#token-err').textContent === 'That token looks too short'));
  token.value = 'a'.repeat(64);
  key(token, { key: 'Enter' });
  check('token saved and cleared', await until(() => token.value === '' && $('#token-err').hidden));
  check('token placeholder', token.placeholder === 'Saved in Keychain');
  const url = $('#server-url');
  url.value = 'share.example.com';
  $('#done').click();
  check('invalid URL keeps Settings open', await until(() => !$('#server-url-err').hidden) && !$('#settings').hidden);
  url.value = 'https://share.example.com/';
  $('#done').click();
  check('valid URL closes Settings', await until(() => $('#settings').hidden));
  check('URL normalized', url.value === 'https://share.example.com');
  await until(() => zone() === 'idle');
  check('hint follows the cleared shortcut', text('#zone-hint') === 'Set a capture shortcut in Settings');

  // Launch at login
  key(doc.body, { key: ',', metaKey: true });
  await until(() => !$('#settings').hidden);
  $('#login').click();
  check('switch turns on', await until(() => $('#login').getAttribute('aria-checked') === 'true'));
  key(doc.body, { key: 'Enter' });
  check('Return closes Settings', await until(() => $('#settings').hidden));

  // navigate event from Rust (tray "Settings…")
  mock.emit('navigate', { view: 'settings' });
  check('navigate event shows Settings', await until(() => !$('#settings').hidden));

  summary();
}
