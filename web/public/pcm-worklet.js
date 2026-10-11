// The microphone's raw samples for live dictation (lib/stt.ts): every block
// the audio graph renders, copied to the page, which resamples it to 16 kHz
// and streams it to `tenx web`. Outputs nothing.
class Pcm extends AudioWorkletProcessor {
  process(inputs) {
    const channel = inputs[0] && inputs[0][0];
    if (channel) this.port.postMessage(channel.slice(0));
    return true;
  }
}
registerProcessor('pcm', Pcm);
