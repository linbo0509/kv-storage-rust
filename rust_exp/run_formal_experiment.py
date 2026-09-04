#!/usr/bin/env python3
"""交替运行 Rust/Java 固定速率实验，并采集服务端 CPU 与内存。"""
from __future__ import annotations
import argparse, csv, json, os, platform, shutil, signal, socket, subprocess, threading, time
from datetime import datetime
from pathlib import Path

ROOT=Path(__file__).resolve().parent

def args():
    p=argparse.ArgumentParser()
    p.add_argument('--rounds',type=int,default=5); p.add_argument('--warmup',type=int,default=10)
    p.add_argument('--duration',type=int,default=30); p.add_argument('--rate',type=int,default=60000)
    p.add_argument('--clients',type=int,default=32); p.add_argument('--keys',type=int,default=50000)
    p.add_argument('--value-size',type=int,default=1024); p.add_argument('--cooldown',type=int,default=3)
    p.add_argument('--output',type=Path)
    return p.parse_args()

def wait_port(port, proc):
    for _ in range(100):
        if proc.poll() is not None: raise RuntimeError(f'服务器提前退出，状态码 {proc.returncode}')
        try:
            with socket.create_connection(('127.0.0.1',port),timeout=.1): return
        except OSError: time.sleep(.1)
    raise RuntimeError(f'端口 {port} 未就绪')

def monitor(pid, output, stop):
    start=time.time()
    with output.open('w',newline='') as f:
        w=csv.writer(f); w.writerow(['elapsed_s','timestamp_ms','cpu_percent','rss_mb'])
        while not stop.is_set():
            r=subprocess.run(['ps','-p',str(pid),'-o','%cpu=,rss='],capture_output=True,text=True)
            if r.returncode==0 and r.stdout.strip():
                cpu,rss=r.stdout.split()[:2]; w.writerow([f'{time.time()-start:.3f}',int(time.time()*1000),cpu,f'{int(rss)/1024:.3f}']); f.flush()
            stop.wait(.25)

def one(kind, run_dir, cfg):
    port=18780 if kind=='rust' else 18781
    run_dir.mkdir(parents=True,exist_ok=True)
    gc=run_dir/'java-gc.log'
    if kind=='rust': cmd=[str(ROOT/'rust-kv/target/release/kv-memory-server'),'--addr',f'127.0.0.1:{port}']
    else: cmd=['java','-Xms256m','-Xmx256m','-XX:+UseG1GC',f'-Xlog:gc*,safepoint:file={gc}:time,uptime,level,tags','-cp',str(ROOT/'java-kv/target/classes'),'experiment.JavaKvServer','--addr',f'127.0.0.1:{port}']
    with (run_dir/f'{kind}-server.log').open('w') as log:
        proc=subprocess.Popen(cmd,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
        stop=threading.Event(); t=None
        try:
            wait_port(port,proc)
            t=threading.Thread(target=monitor,args=(proc.pid,run_dir/f'{kind}-resource.csv',stop),daemon=True); t.start()
            bench=[str(ROOT/'rust-kv/target/release/kv-bench'),'--addr',f'127.0.0.1:{port}','--label',kind,'--clients',str(cfg.clients),'--keys',str(cfg.keys),'--value-size',str(cfg.value_size),'--warmup-seconds',str(cfg.warmup),'--duration-seconds',str(cfg.duration),'--rate',str(cfg.rate),'--output',str(run_dir/f'{kind}.csv')]
            with (run_dir/f'{kind}-bench.log').open('w') as out:
                subprocess.run(bench,stdout=out,stderr=subprocess.STDOUT,check=True)
        finally:
            stop.set()
            if t: t.join(2)
            if proc.poll() is None:
                os.killpg(proc.pid,signal.SIGTERM)
                try: proc.wait(3)
                except subprocess.TimeoutExpired: os.killpg(proc.pid,signal.SIGKILL)

def version(cmd):
    r=subprocess.run(cmd,capture_output=True,text=True); return (r.stdout+r.stderr).strip()

def main():
    c=args(); stamp=datetime.now().strftime('formal-%Y%m%d-%H%M%S'); out=(c.output or ROOT/'results'/stamp).resolve(); out.mkdir(parents=True)
    meta={'started_at':datetime.now().astimezone().isoformat(),'platform':platform.platform(),'machine':platform.machine(),'python':platform.python_version(),'rustc':version(['rustc','--version']),'java':version(['java','-version']),'parameters':vars(c)|{'output':str(out)}}
    (out/'metadata.json').write_text(json.dumps(meta,ensure_ascii=False,indent=2),encoding='utf-8')
    for i in range(1,c.rounds+1):
        order=['rust','java'] if i%2 else ['java','rust']
        for kind in order:
            print(f'第 {i}/{c.rounds} 轮：{kind}',flush=True); one(kind,out/f'run-{i:02d}',c); time.sleep(c.cooldown)
    print(f'RESULT_DIR={out}',flush=True)
if __name__=='__main__': main()
