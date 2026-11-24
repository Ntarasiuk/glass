/**
 * The survey card (~0.9 KB gzipped). Added after the beacon only on pages where this visitor is asked, so every
 * other page pays nothing. Built with DOM calls and textContent: the question and options are never parsed as HTML.
 * Light or dark to match the page's own background, not the visitor's system setting.
 * No cookies, no storage: a reload can ask again; an answer or a dismissal ends it for that page view.
 *
 * Sends $survey_shown, then $survey { survey, answer } or $survey_dismiss, through the beacon's track().
 */
import type { Survey } from "./surveys.ts";

export const SURVEY_JS = `(()=>{var s=document.currentScript,c;try{c=JSON.parse(s.dataset.q)}catch(e){return}
var w=window,d=document,A=w.agentlens;if(!A||!c||!c.i||w.__alq)return;w.__alq=1;
var P=location.pathname,box,done=0;
function ev(n,a){A.track(n,a?{survey:c.i,answer:a}:{survey:c.i})}
function el(t,css,txt){var e=d.createElement(t);e.style.cssText=css;if(txt)e.textContent=txt;return e}
function close(){if(box){box.remove();box=null}}
function answer(a){if(done)return;done=1;ev("$survey",String(a).slice(0,140));box.textContent="";box.appendChild(el("div","padding:2px 0","Thanks."));setTimeout(close,1600)}
function show(){if(box||done||location.pathname!==P||!d.body)return;
var dk=0;try{[d.body,d.documentElement].some(function(n){var m=getComputedStyle(n).backgroundColor.match(/[\\d.]+/g);if(m&&(m.length<4||+m[3]>.5)){dk=(+m[0]*299+ +m[1]*587+ +m[2]*114)/1000<128;return 1}})}catch(e){}
var bg=dk?"#1c1c1e":"#fff",fg=dk?"#f2f2f2":"#111",bd=dk?"#48484a":"#c9c9c9";
box=el("div","position:fixed;z-index:2147483000;right:16px;bottom:16px;width:320px;max-width:calc(100vw - 32px);box-sizing:border-box;padding:14px 16px;border-radius:12px;border:1px solid "+bd+";background:"+bg+";color:"+fg+";font:14px/1.4 system-ui,-apple-system,sans-serif;text-align:left;box-shadow:0 6px 24px rgba(0,0,0,.18)");
box.setAttribute("role","region");box.setAttribute("aria-label","Quick question");
var x=el("button","position:absolute;top:4px;right:6px;border:0;background:none;color:inherit;font:18px/1 system-ui;cursor:pointer;padding:8px","\\u00d7");x.type="button";x.setAttribute("aria-label","Close");x.onclick=function(){if(!done){done=1;ev("$survey_dismiss")}close()};
box.appendChild(x);box.appendChild(el("div","font-weight:600;margin:0 28px 8px 0",c.q));
var bs="border-radius:8px;border:1px solid "+bd+";background:none;color:inherit;font:inherit;cursor:pointer;padding:9px 12px;";
(c.o||[]).forEach(function(o){var b=el("button",bs+"display:block;width:100%;text-align:left;margin:6px 0 0",o);b.type="button";b.onclick=function(){answer(o)};box.appendChild(b)});
if(c.x){var f=el("form","display:flex;gap:6px;margin:8px 0 0"),i=el("input","flex:1;min-width:0;border-radius:8px;border:1px solid "+bd+";background:none;color:inherit;font:inherit;padding:8px 10px");i.maxLength=140;i.placeholder=c.o&&c.o.length?"Something else":"Your answer";i.setAttribute("aria-label",c.q);
var sb=el("button",bs,"Send");sb.type="submit";f.onsubmit=function(e){e.preventDefault();var v=i.value.trim();if(v)answer(v)};f.appendChild(i);f.appendChild(sb);box.appendChild(f)}
d.body.appendChild(box);ev("$survey_shown")}
setTimeout(show,(c.a||0)*1000)})();`;

const attr = (v: string) => v.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** The script tag for one survey. The config rides in an attribute, escaped, and is read as data. */
export function surveyTag(s: Survey): string {
  const cfg = JSON.stringify({ i: s.id, q: s.question, o: s.options, x: s.other ? 1 : 0, a: s.after });
  return `<script data-q="${attr(cfg)}">${SURVEY_JS}</script>`;
}
