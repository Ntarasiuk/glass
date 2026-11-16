/**
 * Client beacon (~1.4 KB gzipped). Inlined into each HTML page (no extra request, nothing for ad-block lists to match);
 * also served at /_al/s.js.
 * No cookies, no storage. Sends: pv on load/route change, engage on hide (time, interaction, scroll depth,
 * Core Web Vitals), custom events, and automatic $error, $rage_click, $outbound, $file_download and $clicks
 * (batched click positions for heatmaps, with dead clicks: no visible effect within 600 ms) events.
 * With #al_heatmap in the URL (the dashboard's heatmap iframe) it tracks nothing and answers element positions.
 *
 *   agentlens.track("signup", { plan: "pro" }, 49)
 *   agentlens.experiment("pricing_v2", "b")   → $exposure, once per page
 *   <button data-al-event="cta_click">  → auto-tracked
 */
export const BEACON_JS = `(()=>{var s=document.currentScript,t=s&&s.dataset.t;if(!t||window.agentlens)return;
var n=navigator,w=window,E="/_al/e",G=[];
function dh(){var h=document.documentElement;return Math.max(h.scrollHeight,document.body?document.body.scrollHeight:0)}
if(/[#&]al_heatmap/.test(location.hash)){w.agentlens={track:function(){},experiment:function(){}};addEventListener("message",function(e){var d=e.data;if(!d||d.al!=="rects"||!Array.isArray(d.s)||!e.source)return;var o={};d.s.slice(0,400).forEach(function(x){try{var el=document.querySelector(String(x));if(el){var r=el.getBoundingClientRect();o[x]=[Math.round(r.left+w.scrollX),Math.round(r.top+w.scrollY),Math.round(r.width),Math.round(r.height)]}}catch(er){}});e.source.postMessage({al:"rects",id:d.id,w:document.documentElement.scrollWidth,h:dh(),r:o},e.origin)});return}
if(n.webdriver)G.push("webdriver");
["__playwright","__pwInitScripts","__puppeteer_evaluation_script__","callPhantom","_phantom","__nightmare","domAutomation","domAutomationController","_selenium","__selenium_unwrapped","__webdriver_evaluate","__driver_evaluate","__lastWatirAlert"].forEach(function(k){if(k in w)G.push("g:"+k)});
try{for(var k in w)if(/^\\$?cdc_/.test(k)){G.push("g:cdc");break}}catch(e){}
if(/HeadlessChrome/.test(n.userAgent))G.push("headless_ua");
if(w.outerWidth===0&&w.outerHeight===0)G.push("zero_outer");
if(n.languages&&n.languages.length===0)G.push("no_languages");
var q=new URLSearchParams(location.search),utm=q.get("utm_source")||"",med=q.get("utm_medium")||"",cmp=q.get("utm_campaign")||"",ref=document.referrer,P=location.pathname;
function send(k,d){var b=JSON.stringify(Object.assign({t:t,k:k,p:P,r:ref,u:utm,m:med,c:cmp,w:w.innerWidth,tp:n.maxTouchPoints||0,s:G},d));try{n.sendBeacon(E,b)||fetch(E,{method:"POST",body:b,keepalive:true})}catch(e){}}
var S;function reset(){S={t0:performance.now(),mv:0,sc:0,ky:0,ck:0,cnm:0,tc:0,ut:0,d:0,fx:-1,fy:-1,lx:-1,ly:-1,sent:0,sd:0}}reset();
var o={passive:true,capture:true},V={lcp:0,inp:0,cls:0,ttfb:0},EC=0,RC=[];
try{var nv=performance.getEntriesByType("navigation")[0];if(nv)V.ttfb=Math.round(nv.responseStart)}catch(e){}
function po(y,f,x){try{new PerformanceObserver(function(l){l.getEntries().forEach(f)}).observe(Object.assign({type:y,buffered:true},x||{}))}catch(e){}}
po("largest-contentful-paint",function(e){V.lcp=Math.round(e.startTime)});
po("layout-shift",function(e){if(!e.hadRecentInput)V.cls+=e.value});
po("event",function(e){if(e.interactionId)V.inp=Math.max(V.inp,Math.round(e.duration))},{durationThreshold:40});
function depth(){var h=document.documentElement,vh=w.innerHeight,dh=Math.max(h.scrollHeight,document.body?document.body.scrollHeight:0);S.sd=Math.max(S.sd,dh>vh?Math.min(100,Math.round((w.scrollY+vh)/dh*100)):100)}
addEventListener("load",depth);
addEventListener("error",function(e){if(EC++<5&&e.message)track("$error",{message:String(e.message).slice(0,180),source:((e.filename||"").split("/").pop()||"inline")+":"+(e.lineno||0)})});
addEventListener("unhandledrejection",function(e){var r=e.reason;if(EC++<5)track("$error",{message:String(r&&r.message||r).slice(0,180),source:"promise"})});
function csel(el){var p=[];for(var i=0;el&&el.nodeType===1&&el!==document.body&&i<7;i++,el=el.parentElement){var x=el.tagName.toLowerCase();if(el.id&&/^[A-Za-z][\\w-]{0,40}$/.test(el.id)){p.unshift("#"+el.id);break}var pe=el.parentElement,k=0,j=0;if(pe)for(var c=pe.firstElementChild;c;c=c.nextElementSibling)if(c.tagName===el.tagName){k++;if(c===el)j=k}p.unshift(k>1?x+":nth-of-type("+j+")":x)}return p.join(">")}
var CB=[],CN=0,MC=0;try{new MutationObserver(function(){MC++}).observe(document.documentElement,{childList:true,subtree:true,attributes:true,characterData:true})}catch(e){}
function flush(){if(CB.length){track("$clicks",{w:w.innerWidth,h:dh(),c:CB});CB=[]}}
function sel(el){var p=[];for(var i=0;el&&el.nodeType===1&&i<3;i++,el=el.parentElement){var x=el.tagName.toLowerCase();if(el.id){p.unshift(x+"#"+el.id);break}var c=(el.getAttribute("class")||"").trim().split(/\s+/)[0];p.unshift(c&&!/[:\[]/.test(c)?x+"."+c:x)}return p.join(" > ").slice(0,120)}
addEventListener("mousemove",function(e){if(!e.isTrusted){S.ut++;return}if(S.lx<0){S.fx=e.clientX;S.fy=e.clientY}else S.d+=Math.hypot(e.clientX-S.lx,e.clientY-S.ly);S.lx=e.clientX;S.ly=e.clientY;S.mv++},o);
addEventListener("scroll",function(){S.sc++;depth()},o);
addEventListener("touchstart",function(){S.tc++},o);
addEventListener("keydown",function(e){e.isTrusted?S.ky++:S.ut++},o);
addEventListener("click",function(e){if(!e.isTrusted){S.ut++;return}S.ck++;if(!S.mv&&!S.tc)S.cnm++;
var tg=e.target&&e.target.closest?e.target:null,el=tg&&tg.closest("[data-al-event]");if(el)track(el.getAttribute("data-al-event"));
var a=tg&&tg.closest("a[href]");if(a){try{var u=new URL(a.href,location.href),f=u.pathname.split("/").pop()||"";
if(/\.(zip|dmg|pkg|exe|msi|deb|rpm|apk|gz|tgz|bz2|xz|7z|rar|pdf|csv|xlsx?|docx?|pptx?|mp3|mp4|mov|iso|img)$/i.test(f))track("$file_download",{file:f.slice(0,120),host:u.host});
else if(/^https?:$/.test(u.protocol)&&u.host!==location.host)track("$outbound",{to:u.host,url:(u.host+u.pathname).slice(0,160)})}catch(x){}}
var now=Date.now();RC=RC.filter(function(c){return now-c.t<1000&&Math.abs(c.x-e.clientX)<30&&Math.abs(c.y-e.clientY)<30});RC.push({t:now,x:e.clientX,y:e.clientY});
if(RC.length===3&&tg)track("$rage_click",{target:sel(tg),text:(tg.innerText||"").trim().slice(0,40)});
if(tg&&CN++<60){var lb=tg.closest("a,button,summary,[role=button],[role=link],[role=tab],[role=menuitem]"),el2=lb||tg,r=el2.getBoundingClientRect(),dw=document.documentElement.scrollWidth||1,
it=[csel(el2),r.width?+Math.min(1,Math.max(0,(e.clientX-r.left)/r.width)).toFixed(2):0,r.height?+Math.min(1,Math.max(0,(e.clientY-r.top)/r.height)).toFixed(2):0,+(e.pageX/dw*100).toFixed(1),Math.round(e.pageY),0,lb?(lb.getAttribute("aria-label")||lb.innerText||"").trim().replace(/\\s+/g," ").slice(0,24):""],m0=MC,u0=location.href,y0=w.scrollY;
CB.push(it);setTimeout(function(){if(MC===m0&&location.href===u0&&w.scrollY===y0&&!tg.closest("input,select,textarea,label,option,video,audio,canvas,iframe,[contenteditable]"))it[5]=1},600);
if(JSON.stringify(CB).length>640)setTimeout(flush,650)}},o);
function engage(){flush();if(S.sent)return;S.sent=1;var disp=S.lx<0?0:Math.hypot(S.lx-S.fx,S.ly-S.fy);
depth();send("engage",{d:Math.round(performance.now()-S.t0),mv:S.mv,sc:S.sc,ky:S.ky,ck:S.ck,cnm:S.cnm,tc:S.tc,ut:S.ut,st:S.d>0?+(disp/S.d).toFixed(3):0,sd:S.sd,lcp:V.lcp,inp:V.inp,cls:Math.round(V.cls*1000),ttfb:V.ttfb});V={lcp:0,inp:0,cls:0,ttfb:0}}
function track(name,props,value){if(name)send("event",{n:String(name).slice(0,64),pr:props||null,v:+value||0})}
var X={};function experiment(name,variant){name=String(name||"").slice(0,48);variant=String(variant==null?"":variant).slice(0,32);if(name&&variant&&X[name]!==variant){X[name]=variant;track("$exposure",{experiment:name,variant:variant})}}
function nav(){if(location.pathname===P)return;engage();ref=location.origin+P;utm=med=cmp="";P=location.pathname;reset();CN=0;X={};depth();send("pv")}
["pushState","replaceState"].forEach(function(m){var f=history[m];history[m]=function(){var r=f.apply(this,arguments);nav();return r}});
addEventListener("popstate",nav);
addEventListener("visibilitychange",function(){if(document.visibilityState==="hidden")engage()});
addEventListener("pagehide",engage);
var Q=w.agentlens_q||[];w.agentlens={track:track,experiment:experiment};Q.forEach(function(a){a[0]==="experiment"?experiment(a[1],a[2]):track.apply(null,a)});
send("pv")})();`;
