#!/usr/bin/env python3
"""汇总多轮实验，生成与参考图一致的 2×2 SVG 性能仪表盘。"""
from __future__ import annotations
import argparse,csv,json,re,statistics
from datetime import datetime
from html import escape
from pathlib import Path
GC=re.compile(r'^\[(?P<time>[^]]+)\].*\[gc\s*\].*Pause .* (?P<ms>[0-9.]+)ms$')
def rows(p):
    with p.open(newline='') as f:return list(csv.DictReader(f))
def medseries(runs,key):
    n=min(map(len,runs));return [statistics.median(float(r[i][key]) for r in runs) for i in range(n)]
def gc_pauses(path,start,end):
    ans=[]
    for line in path.read_text(errors='replace').splitlines():
        m=GC.match(line)
        if m:
            t=round(datetime.fromisoformat(m['time']).timestamp()*1000)
            if start<=t<end:ans.append((t,float(m['ms'])))
    return ans
def path_points(vals,x,y,w,h,ymax):
    n=max(1,len(vals)-1);return ' '.join(f'{x+i*w/n:.1f},{y+h-v/ymax*h:.1f}' for i,v in enumerate(vals))
def dashboard(data,out):
    W,H=1600,870;bg='#ececec';blue='#3b82b7';orange='#e48b35';red='#d84a4a'
    panels=[(18,22,768,385,'System CPU (%)','cpu_percent',1),(814,22,768,385,'Average Response Time (ms)','average_us',.001),(18,438,768,385,'Response Time (95th) (ms)','p95_us',.001),(814,438,768,385,'Max Response Time (ms)','max_us',.001)]
    s=[f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}">',f'<rect width="100%" height="100%" fill="{bg}"/>','<style>text{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Arial,sans-serif}</style>']
    for px,py,pw,ph,title,key,scale in panels:
        x,y,w,h=px+62,py+56,pw-82,ph-94;rv=[v*scale for v in data['rust'][key]];jv=[v*scale for v in data['java'][key]];ymax=max(max(rv,default=1),max(jv,default=1))*1.12 or 1
        s += [f'<rect x="{px}" y="{py}" width="{pw}" height="{ph}" fill="white"/>',f'<text x="{px+16}" y="{py+31}" fill="#7b8794" font-size="22">{escape(title)}</text>']
        for i in range(5):
            yy=y+h-i*h/4;v=ymax*i/4;s += [f'<line x1="{x}" y1="{yy:.1f}" x2="{x+w}" y2="{yy:.1f}" stroke="#edf0f2"/>',f'<text x="{x-10}" y="{yy+5:.1f}" text-anchor="end" fill="#68737d" font-size="13">{v:.1f}</text>']
        s += [f'<line x1="{x}" y1="{y+h}" x2="{x+w}" y2="{y+h}" stroke="#d8dde2"/>',f'<polyline points="{path_points(rv,x,y,w,h,ymax)}" fill="none" stroke="{blue}" stroke-width="2"/>',f'<polyline points="{path_points(jv,x,y,w,h,ymax)}" fill="none" stroke="{orange}" stroke-width="2"/>']
        if key=='max_us':
            for sec in data['gc_seconds']:
                gx=x+sec*w/max(1,len(rv)-1);s.append(f'<line x1="{gx:.1f}" y1="{y}" x2="{gx:.1f}" y2="{y+h}" stroke="{red}" stroke-width="1" stroke-dasharray="4 4" opacity=".35"/>')
        s += [f'<line x1="{px+pw-175}" y1="{py+27}" x2="{px+pw-145}" y2="{py+27}" stroke="{blue}" stroke-width="3"/><text x="{px+pw-139}" y="{py+32}" fill="#68737d" font-size="13">Rust</text>',f'<line x1="{px+pw-90}" y1="{py+27}" x2="{px+pw-60}" y2="{py+27}" stroke="{orange}" stroke-width="3"/><text x="{px+pw-54}" y="{py+32}" fill="#68737d" font-size="13">Java</text>']
    s.append('</svg>');out.write_text('\n'.join(s),encoding='utf-8')
def main():
    p=argparse.ArgumentParser();p.add_argument('result_dir',type=Path);a=p.parse_args();root=a.result_dir.resolve();rr=[];jj=[];rcpu=[];jcpu=[];allgc=[];overall=[]
    for d in sorted(root.glob('run-*')):
        r=rows(d/'rust.csv');j=rows(d/'java.csv');ro,jo=r[-1],j[-1];rs,js=r[:-1],j[:-1];rr.append(rs);jj.append(js)
        for kind,o in [('rust',ro),('java',jo)]:overall.append({'run':d.name,'implementation':kind,**{k:float(o[k]) for k in ['throughput_rps','average_us','p95_us','p99_us','p999_us','max_us']}})
        for kind,data,target in [('rust',rs,rcpu),('java',js,jcpu)]:
            start=int(data[0]['timestamp_ms']);end=int(data[-1]['timestamp_ms'])+1000;samp=[x for x in rows(d/f'{kind}-resource.csv') if start<=int(x['timestamp_ms'])<end];b=[]
            for sec in range(len(data)):
                vals=[float(x['cpu_percent']) for x in samp if sec<=(int(x['timestamp_ms'])-start)/1000<sec+1];b.append(statistics.mean(vals) if vals else 0)
            target.append(b)
        start=int(js[0]['timestamp_ms']);end=int(js[-1]['timestamp_ms'])+1000;allgc += [(round((t-start)/1000),ms,d.name) for t,ms in gc_pauses(d/'java-gc.log',start,end)]
    n=min(map(len,rr+jj));data={'seconds':list(range(n)),'rust':{},'java':{},'gc_seconds':sorted(set(s for s,_,_ in allgc))}
    for name,runs in [('rust',rr),('java',jj)]:
        for k in ['average_us','p95_us','p99_us','p999_us','max_us']:data[name][k]=medseries(runs,k)
    data['rust']['cpu_percent']=[statistics.median(v[i] for v in rcpu) for i in range(n)];data['java']['cpu_percent']=[statistics.median(v[i] for v in jcpu) for i in range(n)]
    (root/'dashboard-data.json').write_text(json.dumps(data,ensure_ascii=False),encoding='utf-8');dashboard(data,root/'performance-dashboard.svg')
    with (root/'overall-results.csv').open('w',newline='') as f:w=csv.DictWriter(f,fieldnames=overall[0]);w.writeheader();w.writerows(overall)
    def med(kind,k):return statistics.median(r[k] for r in overall if r['implementation']==kind)
    rounds=len(rr);lines=['# 实验摘要','',f'采用固定速率开环负载；表中为 {rounds} 轮总体结果的中位数。','','| 指标 | Rust | Java G1 | Java / Rust |','|---|---:|---:|---:|']
    for label,k,u in [('吞吐量','throughput_rps',' req/s'),('平均延迟','average_us',' us'),('P95','p95_us',' us'),('P99','p99_us',' us'),('P99.9','p999_us',' us'),('最大延迟','max_us',' us')]:
        r,j=med('rust',k),med('java',k);lines.append(f'| {label} | {r:.2f}{u} | {j:.2f}{u} | {j/r:.2f}× |')
    lines += ['',f'- 测量窗口内 Java GC 暂停：{len(allgc)} 次',f'- GC 暂停总计：{sum(x[1] for x in allgc):.3f} ms',f'- 最大单次 GC 暂停：{max((x[1] for x in allgc),default=0):.3f} ms','', '> 红色虚线是 Java GC 暂停所在秒；逐秒聚合只能说明时间相关性。']
    (root/'formal-summary.md').write_text('\n'.join(lines)+'\n',encoding='utf-8');print(root/'performance-dashboard.svg')
if __name__=='__main__':main()
