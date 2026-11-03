// Unit tests for src/lib/voice.ts: `node --test tests/voice.test.ts`.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { CAPTIONS, addCaption, toolLabel, voiceEvent } from '../src/lib/voice.ts';

const rtvi = (type: string, data: Record<string, unknown> = {}) => ({ label: 'rtvi-ai', type, data });

test('the conversation state follows who is speaking', () => {
  assert.deepEqual(voiceEvent(rtvi('bot-ready')), { kind: 'state', state: 'listening' });
  assert.deepEqual(voiceEvent(rtvi('user-started-speaking')), { kind: 'state', state: 'hearing' });
  assert.deepEqual(voiceEvent(rtvi('bot-llm-started')), { kind: 'state', state: 'thinking' });
  assert.deepEqual(voiceEvent(rtvi('bot-started-speaking')), { kind: 'state', state: 'speaking' });
  assert.deepEqual(voiceEvent(rtvi('bot-stopped-speaking')), { kind: 'state', state: 'listening' });
});

test('captions: final transcripts, spoken sentences, tool calls', () => {
  assert.deepEqual(voiceEvent(rtvi('user-transcription', { text: ' what needs me ', final: true })), {
    kind: 'caption',
    caption: { who: 'you', text: 'what needs me' },
  });
  assert.equal(voiceEvent(rtvi('user-transcription', { text: 'what', final: false })), null);
  assert.deepEqual(voiceEvent(rtvi('bot-output', { text: 'Two tasks.', spoken: true, aggregated_by: 'sentence' })), {
    kind: 'caption',
    caption: { who: 'tenx', text: 'Two tasks.' },
  });
  assert.equal(voiceEvent(rtvi('bot-output', { text: 'Two', aggregated_by: 'word' })), null);
  assert.equal(voiceEvent(rtvi('bot-output', { text: 'Two tasks.', spoken: false })), null);
  assert.deepEqual(voiceEvent(rtvi('llm-function-call-started', { function_name: 'tenx_send_message' })), {
    kind: 'caption',
    caption: { who: 'tool', text: 'send message' },
  });
});

test('signalling asks for a new offer; anything else is ignored', () => {
  assert.deepEqual(voiceEvent({ type: 'signalling', message: { type: 'renegotiate' } }), { kind: 'renegotiate' });
  assert.equal(voiceEvent({ type: 'signalling', message: { type: 'peerLeft' } }), null);
  assert.equal(voiceEvent({ label: 'other', type: 'bot-ready' }), null);
  assert.equal(voiceEvent(rtvi('metrics')), null);
  assert.equal(voiceEvent('ping'), null);
});

test('captions join a speaker’s lines and keep only the last few', () => {
  let list = addCaption([], { who: 'tenx', text: 'Two tasks need you.' });
  list = addCaption(list, { who: 'tenx', text: 'The web task asks.' });
  assert.deepEqual(list, [{ who: 'tenx', text: 'Two tasks need you. The web task asks.' }]);
  list = addCaption(list, { who: 'tool', text: 'list tasks' });
  list = addCaption(list, { who: 'tool', text: 'read screen' });
  assert.equal(list.length, 3, 'tool calls stay separate');
  for (let i = 0; i < 10; i++) list = addCaption(list, { who: i % 2 ? 'you' : 'tenx', text: String(i) });
  assert.equal(list.length, CAPTIONS);
  assert.equal(toolLabel('tenx_get_output'), 'get output');
});
