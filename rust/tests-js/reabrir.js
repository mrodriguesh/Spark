// node rust/tests-js/reabrir.js — test/reabrir.js retargeted at the Rust app: REAL Windows Task Scheduler and real
// relaunches by the task, on a temp profile (its own task name), fake Tinder page, offline. No accelerated clock:
// the "deadline passed" phase rewrites likesWaitFrom in spark.json instead (the original used a clock offset).
// Integration (real Windows), simulation of Tinder (fake page with the unlimited-likes offer = limit reached).
const fs = require('fs'), os = require('os'), path = require('path'), assert = require('assert'), crypto = require('crypto')
const { spawn, execFile } = require('child_process')
const { until, sleep, processTree, killProfile, ROOT, EXE } = require('./lib')

const ps = text => "'" + text.replace(/'/g, "''") + "'"
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'spark-reopen-rs-'))
const data = path.join(root, "perfil d' teste $"); fs.mkdirSync(data)
const name = 'Spark - Intervalo - ' + crypto.createHash('sha256').update(data.toLowerCase()).digest('hex').slice(0, 16)
const config = () => JSON.parse(fs.readFileSync(path.join(data, 'spark.json'), 'utf8'))
function windows(script) {
  const code = `$ErrorActionPreference='Stop'; $s=New-Object -ComObject 'Schedule.Service'; $s.Connect(); $r=$s.GetFolder('\\'); $name=${ps(name)}; ` + script
  return new Promise((resolve, reject) => execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(code, 'utf16le').toString('base64')], { windowsHide: true, timeout: 30000 },
    (error, out, err) => error ? reject(new Error(err || error.message)) : resolve(JSON.parse(out.trim()))))
}
const readTask = () => windows(`foreach($t in $r.GetTasks(1)){if($t.Name -eq $name){@{exe=$t.Definition.Actions.Item(1).Path; args=$t.Definition.Actions.Item(1).Arguments; next=([DateTimeOffset]$t.NextRunTime).ToUnixTimeMilliseconds(); missed=$t.Definition.Settings.StartWhenAvailable; logon=$t.Definition.Triggers.Item(2).Type} | ConvertTo-Json -Compress; exit}}; 'null'`)
const runTask = () => windows(`$null=$r.GetTask($name).Run($null); 'null'`)
const running = () => new Promise(res => execFile('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
  `@(Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -and $_.CommandLine.IndexOf(${ps(data)}, [StringComparison]::OrdinalIgnoreCase) -ge 0 }).Count`],
  { windowsHide: true }, (e, out) => res(Number(String(out).trim()) || 0)))
async function ended(label) { await until(async () => (await running()) === 0, label, 90000) }
async function started(label) { await until(async () => (await running()) > 0, label, 60000) }

;(async () => {
  try {
    const page = path.join(root, 'fake.html')
    fs.writeFileSync(page, fs.readFileSync(path.join(ROOT, 'rust', 'app', 'tests', 'fixtures', 'fake-tinder.html'), 'utf8').replace('location.search', JSON.stringify('?offer=initial')))
    fs.writeFileSync(path.join(data, 'spark-test.json'), JSON.stringify({ SPARK_TEST_PACKAGED: '1', SPARK_TEST_FAKE_PAGE: page }))
    fs.writeFileSync(path.join(data, 'spark.json'), JSON.stringify({ enabled: true, autostart: false, startMinimized: false, retryMin: 15,
      skipFirst: false, likesTotal: 20, likesToday: 4, matches: 2, day: new Date().toDateString() }))
    assert.strictEqual(await readTask(), null, 'sem tarefa antes do teste')

    // 1. limite: agenda no Windows e encerra todos os processos (aberto com o Windows = --hidden: fora da tela)
    const env = { ...process.env }; delete env.ELECTRON_RUN_AS_NODE
    const first = spawn(EXE, [`--user-data-dir=${data}`, '--hidden'], { env, windowsHide: true, stdio: 'ignore' })
    const tree = await until(async () => { const t = await processTree(first.pid); return t.length > 1 && t }, 'processos da 1ª etapa', 30000)
    const exit = await until(() => first.exitCode === null ? null : { code: first.exitCode, signal: first.signalCode }, 'encerrar a primeira etapa', 90000)
    assert.deepStrictEqual(exit, { code: 0, signal: null })
    await ended('fechar todos os processos da fase 1')
    for (const pid of tree) assert.throws(() => process.kill(pid, 0), { code: 'ESRCH' }, `processo ${pid} permaneceu`)
    const c1 = config(), due1 = c1.likesWaitFrom + 15 * 60000
    const t1 = await readTask()
    assert.ok(t1 && t1.args.includes('--spark-retry-at=' + due1) && t1.args.includes('--user-data-dir=') && t1.exe === EXE, JSON.stringify(t1))
    assert.ok(t1.missed && t1.logon === 9 && Math.abs(t1.next - due1) < 1000)
    assert.strictEqual(c1.likesTotal, 20)
    console.log('ok limite: agendou no Windows e encerrou todos os processos')

    // 2. o Windows reabre antes do prazo: não dá likes, reagenda o mesmo prazo e encerra
    await runTask(); await started('reabertura pelo Windows'); await ended('encerrar a fase 2')
    const c2 = config()
    assert.strictEqual(c2.likesWaitFrom, c1.likesWaitFrom, 'voltar antes do prazo não consome o prazo salvo')
    assert.strictEqual(c2.likesTotal, 20)
    assert.ok((await readTask()).args.includes('--spark-retry-at=' + due1))
    console.log('ok retorno antes do prazo: encerrou sem curtidas, tarefa mantida')

    // 3. prazo vencido: a tentativa roda, detecta o limite de novo, reagenda e encerra
    fs.writeFileSync(path.join(data, 'spark.json'), JSON.stringify({ ...c2, likesWaitFrom: Date.now() - 20 * 60000 }))
    const old = config().likesWaitFrom
    await runTask(); await started('reabertura vencida'); await ended('encerrar a fase 3')
    const c3 = config(), due3 = c3.likesWaitFrom + 15 * 60000
    assert.ok(c3.likesWaitFrom > old, 'nova detecção do limite')
    assert.ok((await readTask()).args.includes('--spark-retry-at=' + due3))
    assert.strictEqual(c3.likesTotal, 20)
    console.log('ok reabertura vencida: tentou, detectou o limite, reagendou e encerrou')

    // 4. pausa persistida: a tarefa abre, não abre janela, remove a tarefa e encerra
    fs.writeFileSync(path.join(data, 'spark.json'), JSON.stringify({ ...c3, enabled: false }))
    await runTask(); await ended('encerrar a fase 4')
    await until(async () => (await readTask()) === null, 'remover a tarefa ao estar pausado', 30000)
    console.log('ok pausa persistida bloqueou e removeu a tarefa')
    console.log('REABERTURA OK')
  } catch (e) { console.error('FALHOU:', e.message); process.exitCode = 1 }
  finally {
    try { await windows(`foreach($t in $r.GetTasks(1)){if($t.Name -eq $name){$r.DeleteTask($name,0)}}; 'null'`) } catch {}
    await killProfile(data); await sleep(1000)
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 })
  }
})()
