'use strict';
// Run inside an agent's blocking terminal tool; returns when its peer posts.
const fs = require('node:fs');
const path = require('node:path');
const agent = process.argv[2];
if (!['Claude', 'Codex'].includes(agent)) {
  console.error('Usage: node tools/collab-monitor/watch.cjs Claude|Codex [timeout-seconds]');
  process.exit(2);
}
const seconds = Number(process.argv[3] || 55);
if (!Number.isFinite(seconds) || seconds < 1 || seconds > 3600) process.exit(2);
const peer = agent === 'Claude' ? 'Codex' : 'Claude';
const file = path.resolve(__dirname, '../../AGENT_HANDOFF.md');
function messages() {
  const text = fs.readFileSync(file, 'utf8');
  const section = text.split('## Messages and handoffs')[1]?.split(/\n## /)[0] || '';
  return section.split(/\r?\n(?=- )/).map(s => s.trim()).filter(s => new RegExp('^-(?: \\d{4}-\\d{2}-\\d{2} /)? '+peer+' /').test(s));
}
const seen = new Set(messages());
console.log(`Watching for new ${peer} messages for ${seconds}s.`);
const interval = setInterval(() => {
  try {
    const added = messages().filter(s => !seen.has(s));
    if (added.length) { console.log(added.join('\n\n')); clearInterval(interval); clearTimeout(timeout); }
  } catch (error) { console.error(error.message); clearInterval(interval); clearTimeout(timeout); process.exitCode = 1; }
}, 1000);
const timeout = setTimeout(() => { clearInterval(interval); console.log('No new peer messages in this interval.'); }, seconds * 1000);
