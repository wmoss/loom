import test from 'node:test';
import assert from 'node:assert/strict';
import { cancelConfirmation, confirmAction, confirmation } from './confirmation.ts';

test('an option starts unchecked unless its caller asks otherwise', async () => {
  confirmAction({
    title: 'Archive this session?',
    description: '',
    confirmLabel: 'Archive session',
    option: { label: 'Also archive 1 child session' },
    action: async () => {},
  });
  assert.equal(confirmation.optionChecked, false);
  cancelConfirmation();

  confirmAction({
    title: 'Archive this session?',
    description: '',
    confirmLabel: 'Archive session',
    option: { label: 'Also archive 1 child session', checked: true },
    action: async () => {},
  });
  assert.equal(
    confirmation.optionChecked,
    true,
    'the archive prompt pre-ticks the child checkbox when the setting is on',
  );
  cancelConfirmation();
});
