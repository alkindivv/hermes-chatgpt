// Derived from codex-chatgpt-web v6.1.10 (MIT); see LICENSE.upstream.
// Executed only in the explicitly supplied application-owned page target.
async function page(op, arg) {
  const abortGeneration=globalThis.__CW_BROWSER_ABORT_GENERATION__||0;
  const composerSelector = '[data-testid="prompt-textarea"], #prompt-textarea, [contenteditable="true"][data-lexical-editor="true"], form[data-chatgpt-composer] [data-composer-markdown][contenteditable="true"][role="textbox"]';
  const visible = e => e.isConnected && !e.closest('[hidden],[inert],[aria-hidden="true"]') && getComputedStyle(e).visibility !== 'hidden' && getComputedStyle(e).display !== 'none' && e.getClientRects().length > 0;
  const all = (selector, root = document) => [...root.querySelectorAll(selector)];
  const one = (selector, root = document) => { const a=all(selector,root).filter(visible); if(a.length!==1) throw Error('Expected one owned control: '+selector); return a[0]; };
  if(op==='ready')return location.origin==='https://chatgpt.com'&&document.readyState==='complete'&&all(composerSelector).filter(visible).length===1&&!!all(composerSelector).filter(visible)[0].closest('form');
  if (location.origin !== 'https://chatgpt.com') throw Error('Not a ChatGPT page');
  const settle=async(ms=150)=>{await new Promise(r=>setTimeout(r,ms));if(abortGeneration!==(globalThis.__CW_BROWSER_ABORT_GENERATION__||0))throw Error('Owned operation cancelled');};
  const serverSession=async()=>{
    const response=await fetch('/api/auth/session',{credentials:'same-origin',cache:'no-store',redirect:'error',signal:AbortSignal.timeout(5000)});
    const url=new URL(response.url);
    if(url.origin!=='https://chatgpt.com'||url.pathname!=='/api/auth/session')throw Error('Server session not verified');
    if(response.status===401)throw Object.assign(Error('Server session signed out'),{signedOut:true});
    if(!response.ok)throw Error('Server session not verified');
    const session=await response.json();
    if(!session||typeof session!=='object'||Array.isArray(session))throw Error('Invalid server session response');
    if((session.error!=null&&session.error!=='')||(session.expires!=null&&(typeof session.expires!=='string'||!Number.isFinite(Date.parse(session.expires))||Date.parse(session.expires)<=Date.now())))throw Error('Server session expired');
    if(session.user==null)throw Object.assign(Error('Server user signed out'),{signedOut:true});
    if(typeof session.user.id!=='string'||!session.user.id)throw Error('Server user identity missing');
    return session;
  };
  if(op==='auth') {
    let authenticated=false,signedOut=false;try{await serverSession();authenticated=true;}catch(error){signedOut=error?.signedOut===true;}
    return {readable:document.readyState!=='loading',composer:all(composerSelector).some(visible),server_authenticated:authenticated,signed_out_confirmed:signedOut};
  }
  if(op==='account'){
    const session=await serverSession();const account=session.account;
    if([session.user.id,account?.id,account?.planType,account?.structure].some(v=>typeof v!=='string'||!v||v.length>256))throw Error('Server account identity incomplete');
    // Only these identity fields leave the page. Never session tokens/cookies.
    return {user_id:session.user.id,account_id:account.id,plan_type:account.planType,structure:account.structure,needs_attention:account.isDelinquent===true,server_authenticated:true};
  }
  const rendered = e => { if(!e.isConnected)return false; for(let n=e;n;n=n.parentElement){const style=getComputedStyle(n);if(n.hidden||style.display==='none'||style.visibility==='hidden'||style.opacity==='0')return false;}return true; };
  const messageText = root => {
    const bubble=root.matches('[data-user-message-bubble]')?root:root.querySelector('[data-user-message-bubble]');
    const expand=bubble?.querySelector('button[type="button"][aria-expanded="false"][data-thread-find-skip="true"][data-markdown-copy="exclude"]');if(expand){expand.click();return {text:null,tokens:[],pending:true};}
    const targets=bubble?[...bubble.querySelectorAll('[data-search-result-target]:not([data-search-result-target] [data-search-result-target])')]:[root.querySelector('[data-message-author-role="user"]')||root];
    if(targets.length!==1)return {text:null,tokens:[]};const observed=readChatGptUserMessage(targets[0]);return {text:observed?.text??null,tokens:literalTokenReceipts(targets[0].textContent||'',arg?.turn_token)};
  };
  const composer = () => one(composerSelector);
  const form = () => { const f=composer().closest('form'); if(!f) throw Error('Composer form missing'); return f; };
  const connectorPills='[data-id^="plugin:"][data-keyword], [app-mention-path^="app://"][app-mention-display-name][contenteditable="false"]';
  const composerText = c => {
    if(typeof c.value==='string')return c.value;
    const copy=c.cloneNode(true);for(const part of all(connectorPills+', svg, style, script, [data-inline-selection-pill-cursor-target]',copy))part.remove();
    const block=n=>/^(P|DIV)$/.test(n.nodeName);
    const read=n=>{
      if(n.nodeType===Node.TEXT_NODE)return n.textContent||'';
      if(n.nodeName==='BR')return '\n';
      const children=[...n.childNodes];
      // A sole BR is the editor's empty paragraph placeholder.
      if(children.length===1&&children[0].nodeName==='BR')return '';
      return children.map((child,i)=>(i>0&&(block(child)||block(children[i-1]))?'\n':'')+read(child)).join('');
    };
    return read(copy);
  };
  const selectedConnectors=c=>all(connectorPills,c).map(e=>({name:e.getAttribute('data-keyword')??e.getAttribute('app-mention-display-name'),path:e.getAttribute('data-id')??e.getAttribute('app-mention-path')}));
  const attachmentTiles=f=>all('[role="group"][aria-label], .composer-attachment-surface:is(button,[role="button"])[aria-label]',f).filter(visible);
  const uploading=f=>all('[role="progressbar"], [data-state="uploading"]',f).some(visible);
  const hasAttachments=f=>attachmentTiles(f).length>0||all('input[type="file"]',f).some(e=>e.files?.length)||uploading(f);
  if(op==='connectorSnapshot') {
    const c=composer();
    const selected=selectedConnectors(c);
    const rows=all('.__menu-item[tabindex="0"], [data-mention-list-scroll-area] button[data-list-navigation-item="true"]').filter(visible).map(e=>{
      const exact=all('*',e).some(child=>visible(child)&&(child.innerText||child.textContent||'').trim()===arg.name);
      return {name:exact?arg.name:(e.innerText||e.textContent||'').split('\n')[0].replace(/\s+/g,' ').trim(),highlighted:e.hasAttribute('data-highlighted')||e.getAttribute('aria-current')==='true',disabled:e.disabled===true||e.getAttribute('aria-disabled')==='true'};
    });
    const running=all('[data-testid="stop-button"], [data-is-streaming="true"]',form()).some(visible);
    return {document_id:String(performance.timeOrigin),text:composerText(c),selected,rows,running,has_attachments:hasAttachments(form())};
  }
  if(op==='connectorFocus') {
    if(String(performance.timeOrigin)!==arg.document_id)throw Error('Connector document ownership changed');
    composer().focus();return true;
  }
  const controlSelector='button[aria-haspopup="menu"][data-tone="neutral"], button[data-testid="model-switcher-dropdown-button"][aria-haspopup="menu"], button[data-codex-intelligence-trigger="true"][data-composer-navigation-target="reasoning"][aria-haspopup="menu"]';
  const sliderSelector='[data-model-reasoning-effort-slider] [role="slider"], [data-model-picker-power-slider] [role="slider"]';
  const thinkSelector='button[type="button"].__composer-pill[aria-pressed]:has(> .__composer-pill-icon > svg[viewBox="0 0 24 24"] > path[d^="M14.8974 2.29998C15.8303 2.29013 16.802 2.58194 17.5566 3.22577"]), button[type="button"][aria-pressed]:has(> svg[viewBox="0 0 20 20"] > use[href$="#brain-regular-20"])';
  const thinkButtons=f=>[...new Set([...all(thinkSelector,f),...all('button',f).filter(e=>(e.getAttribute('aria-label')||e.textContent).trim()==='Think')])].filter(visible);
  const ownedMenu=control=>{
    const id=control.getAttribute('aria-controls');if(id){const menu=document.getElementById(id);if(!menu||!visible(menu))throw Error('Owned model menu missing');return menu;}
    const controlId=control.id;
    const selector=controlId?'[role="menu"][aria-labelledby~='+JSON.stringify(controlId)+']':'[data-testid="composer-intelligence-picker-content"]:has([role="menuitemradio"], [data-model-reasoning-effort-slider]), [role="menu"]:has([role="menuitemradio"], [data-model-reasoning-effort-slider], [data-model-picker-power-slider]), [role="group"]:has([role="menuitemradio"], [data-model-reasoning-effort-slider])';
    return one(selector);
  };
  const openPicker=async()=>{
    const control=one(controlSelector,form());let menu;
    try{menu=ownedMenu(control);}catch{}
    if(!menu||control.getAttribute('aria-expanded')==='false'||control.getAttribute('data-state')==='closed'){control.click();await settle();}
    return {control,menu:ownedMenu(control)};
  };
  const sliderAvailability=menu=>{
    const slider=one(sliderSelector,menu),container=slider.closest('[data-model-reasoning-effort-slider], [data-model-picker-power-slider]');
    if(['aria-valuemin','aria-valuemax','aria-valuenow'].some(a=>!/^[-+]?\d+$/.test(slider.getAttribute(a)||'')))throw Error('Missing effort range');
    const min=Number(slider.getAttribute('aria-valuemin')),max=Number(slider.getAttribute('aria-valuemax')),value=Number(slider.getAttribute('aria-valuenow'));
    if(max<min||max-min>4||value<min||value>max)throw Error('Invalid effort range');
    const ticks=all('[data-selected]',container),power=container.hasAttribute('data-model-picker-power-slider')&&!!container.querySelector('[data-orientation="horizontal"][aria-disabled="false"]');
    const locks=ticks.map(t=>t.getAttribute('data-locked')??(power?'false':null));
    if(locks.length!==max-min+1||locks.some(v=>v!=='true'&&v!=='false'))throw Error('Effort availability not proven');
    return {slider,min,max,value,available:locks.map(v=>v==='false')};
  };
  const familyRegex=family=>family==='5.6'?/^GPT[-\s]?5\.6\s+Sol(?:\s+Pro)?$/i:/^(?:Latest|最新|최신|(?:GPT[-\s]?)?6(?:\s+Astra)?(?:\s+Pro)?)$/i;
  if(op==='capabilityState'){
    const composers=all(composerSelector).filter(visible),f=composers.length===1?composers[0].closest('form'):null;
    const think=f?thinkButtons(f):[];
    return {composer_ready:!!f,document_ready:document.readyState==='complete',selector_count:f?all(controlSelector,f).filter(visible).length:0,think_available:think.length===1&&['true','false'].includes(think[0].getAttribute('aria-pressed'))};
  }
  if(op==='capabilityPicker'){
    const {control,menu}=await openPicker();
    try{
      const {available}=sliderAvailability(menu);
      const families=['5.6','6'].filter(family=>all('[role="menuitemradio"]',menu).filter(e=>familyRegex(family).test((e.getAttribute('aria-label')||e.textContent).trim())&&e.getAttribute('aria-disabled')!=='true'&&!e.disabled).length===1);
      return {available,families};
    }finally{control.click();}
  }
  if(op==='think'){
    const f=form();if(all(controlSelector,f).some(visible))throw Error('Luna unavailable while model selector is present');
    const controls=thinkButtons(f);if(!controls.length&&!arg.enabled)return true;if(!controls.length)return false;
    if(controls.length!==1)throw Error('Ambiguous Think control');const control=controls[0],target=arg.enabled?'true':'false';
    if(!['true','false'].includes(control.getAttribute('aria-pressed')))throw Error('Think pressed state missing');
    if(control.getAttribute('aria-pressed')===target)return true;
    const state=()=>{const c=composer(),copy=c.cloneNode(true);const pills=[...copy.querySelectorAll('[data-id^="plugin:"][data-keyword], [app-mention-path^="app://"][app-mention-display-name][contenteditable="false"]')];const connectors=pills.map(e=>e.getAttribute('data-id')||e.getAttribute('app-mention-path')).sort();pills.forEach(e=>e.remove());return {text:(c.value??copy.textContent??'').trim(),connectors};};
    const before=state();if(before.text)throw Error('Think selection requires an empty draft');control.click();await settle(100);
    const after=state();if(after.text||JSON.stringify(before.connectors)!==JSON.stringify(after.connectors))throw Error('Think changed draft or connector');
    const updated=thinkButtons(form());if(updated.length>1)throw Error('Ambiguous Think control');return updated.length===1&&updated[0].getAttribute('aria-pressed')===target;
  }
  const stopSelector = '[data-testid="stop-button"], form[data-chatgpt-composer] button[type="button"].size-token-button-composer:has(svg.icon-primary-action > path[d="M4.5 5.75C4.5 5.05964 5.05964 4.5 5.75 4.5H14.25C14.9404 4.5 15.5 5.05964 15.5 5.75V14.25C15.5 14.9404 14.9404 15.5 14.25 15.5H5.75C5.05964 15.5 4.5 14.9404 4.5 14.25V5.75Z"])';
  if(op==='checkpointStop'||op==='checkpointIdle') {
    if(String(performance.timeOrigin)!==arg.document_id)throw Error('Checkpoint document ownership changed');
    const stops=all(stopSelector,form()).filter(visible);
    if(op==='checkpointIdle')return stops.length===0;
    globalThis.__CW_BROWSER_ABORT_GENERATION__=(globalThis.__CW_BROWSER_ABORT_GENERATION__||0)+1;
    if(stops.length===1){stops[0].click();return true;}
    if(stops.length>1)throw Error('Ambiguous checkpoint Stop control');
    // No Stop is not by itself evidence of cleanup. Rust must prove the owned final boundary.
    return false;
  }
  if(op==='focus') {if(arg?.document_id&&String(performance.timeOrigin)!==arg.document_id)throw Error('Composer document ownership changed');const c=composer(); if(composerText(c).trim()) throw Error('Composer contains an existing draft'); c.focus(); return true;}
  if(op==='verifyPrompt') { const c=composer(); if(composerText(c)!==arg) throw Error('Typed prompt integrity mismatch'); return true; }
  if(op==='send') {
    // No await may separate the final integrity checks from the click.
    if(!arg||String(performance.timeOrigin)!==arg.document_id)throw Error('Send document ownership changed');
    const c=composer(),f=form(),selected=selectedConnectors(c);
    if(typeof arg.full_prompt!=='string'||composerText(c)!==arg.full_prompt)throw Error('Typed prompt integrity mismatch');
    if(arg.connector_name===null) {
      if(selected.length||arg.connector_path!==null)throw Error('Unexpected connector at Send');
    } else if(typeof arg.connector_name!=='string'||typeof arg.connector_path!=='string'||selected.length!==1
      ||selected[0].name!==arg.connector_name||selected[0].path!==arg.connector_path)throw Error('Selected connector changed before Send');
    if(uploading(f)||all(stopSelector,f).some(visible))throw Error('Composer is not idle for Send');
    const b=one('[data-testid="send-button"], button[type="submit"]',f);
    if(b.disabled||b.getAttribute('aria-disabled')==='true')throw Error('Send disabled');
    b.click();return true;
  }
  if(op==='stop') {one(stopSelector,form()).click();return true;}
  if(op==='model') {
    const opened=await openPicker(),control=opened.control,menu=()=>ownedMenu(control);
    let m=menu(); const re=arg.family==='5.6'?/^GPT[-\s]?5\.6\s+Sol(?:\s+Pro)?$/i:/^(?:Latest|最新|최신|(?:GPT[-\s]?)?6(?:\s+Astra)?(?:\s+Pro)?)$/i;
    const option=()=>{const a=all('[role="menuitemradio"]',menu()).filter(e=>re.test(e.textContent.trim()));if(a.length!==1)throw Error('Exact model family unavailable');return a[0];};
    if(arg.family&&option().getAttribute('aria-checked')!=='true') {
      const views=all('[data-model-picker-view]',m);
      if(views.length===1 && views[0].getAttribute('data-model-picker-view')==='simple') {one('[data-model-picker-view-toggle="true"]:not([aria-hidden="true"])',views[0]).click();await settle();}
      else if(!views.length){const t=one('[role="menuitem"][aria-expanded][aria-hidden="false"]',m);if(t.getAttribute('aria-expanded')==='false'){t.click();await settle();}}
      if(!visible(option())) throw Error('Model family row hidden');option().click();await settle();m=menu();
    }
    const slider=one('[data-model-reasoning-effort-slider] [role="slider"], [data-model-picker-power-slider] [role="slider"]',m);
    if(['aria-valuemin','aria-valuemax','aria-valuenow'].some(a=>!/^[-+]?\d+$/.test(slider.getAttribute(a)||'')))throw Error('Missing effort range');
    const min=Number(slider.getAttribute('aria-valuemin')),max=Number(slider.getAttribute('aria-valuemax'));
    if(!Number.isInteger(min)||!Number.isInteger(max)||max-min<0||max-min>4||arg.index>max-min)throw Error('Unsupported effort range');
    const container=slider.closest('[data-model-reasoning-effort-slider], [data-model-picker-power-slider]');
    const ticks=all('[data-selected]',container);const power=container.hasAttribute('data-model-picker-power-slider')&&!!container.querySelector('[data-orientation="horizontal"][aria-disabled="false"]');
    if(ticks.length!==max-min+1||ticks.some(t=>!['true','false'].includes(t.getAttribute('data-locked')??(power?'false':null)))||(ticks[arg.index].getAttribute('data-locked')??(power?'false':null))!=='false')throw Error('Effort availability not proven');
    (slider.closest('[role="menuitem"]')||slider).focus();return {min,max,value:Number(slider.getAttribute('aria-valuenow'))};
  }
  if(op==='verifyModel'||op==='effortState') {
    const c=one(controlSelector,form());const m=ownedMenu(c);
    const s=one('[data-model-reasoning-effort-slider] [role="slider"], [data-model-picker-power-slider] [role="slider"]',m);
    if(op==='effortState')return {min:Number(s.getAttribute('aria-valuemin')),value:Number(s.getAttribute('aria-valuenow'))};
    if(Number(s.getAttribute('aria-valuenow'))!==Number(s.getAttribute('aria-valuemin'))+arg.index)throw Error('Effort mismatch');
    if(!arg.family){c.click();return true;}
    const re=arg.family==='5.6'?/^GPT[-\s]?5\.6\s+Sol(?:\s+Pro)?$/i:/^(?:Latest|最新|최신|(?:GPT[-\s]?)?6(?:\s+Astra)?(?:\s+Pro)?)$/i;
    const opts=all('[role="menuitemradio"]',m).filter(e=>re.test(e.textContent.trim()));
    if(opts.length!==1||opts[0].getAttribute('aria-checked')!=='true')throw Error('Family mismatch');
    const descriptions=(s.closest('[role="menuitem"]')?.getAttribute('aria-describedby')||'').split(/\s+/).filter(Boolean).map(id=>document.getElementById(id)?.textContent||'');
    for(const h of all('[data-model-picker-view-toggle="true"]',m).filter(visible)){
      const words=[];const w=document.createTreeWalker(h.querySelector('[data-menu-row-content]')||h,NodeFilter.SHOW_TEXT);for(let n=w.nextNode();n;n=w.nextNode())if(n.parentElement&&visible(n.parentElement)&&n.textContent.trim())words.push(n.textContent.trim());descriptions.push(words.join(' '));
    }
    const states=descriptions.map(t=>/^(?:GPT[-\s]?)?(\d+(?:\.\d+)?)(?:\s+(Sol|Astra))?\s+([^\p{P}]+)(?:\p{P}|$)/iu.exec(t.normalize('NFKC').replace(/\p{Cf}/gu,'').replace(/\s+/g,' ').trim())).filter(Boolean);
    if(!states.length||states.some(s=>s[1]!==arg.family||(s[2]&&s[2].toLowerCase()!==(arg.family==='6'&&arg.index===4?'astra':'sol'))||(arg.index===4?!/^Pro$/i.test(s[3].trim()):/^Pro$/i.test(s[3].trim()))))throw Error('Model announcement mismatch');
    c.click(); return true;
  }
  if(op==='clearAttachments') {
    if(!arg||String(performance.timeOrigin)!==arg.document_id||!Array.isArray(arg.names))throw Error('Attachment cleanup ownership missing');
    const f=form(),tiles=attachmentTiles(f),inputs=all('input[type="file"]',f);
    if(tiles.some(tile=>!arg.names.includes(tile.getAttribute('aria-label')))
      ||inputs.some(input=>[...input.files].some(file=>!arg.names.includes(file.name))))throw Error('Unowned attachment prevents cleanup');
    // Unknown/localized controls remain unconfirmed; never click a guessed action.
    const removals=tiles.map(tile=>one('button[aria-label="Remove file"], button[aria-label="Remove attachment"], button[data-testid="remove-attachment"], button[data-testid="remove-file"]',tile));
    for(const remove of removals)remove.click();
    for(const input of inputs)input.value='';
    const deadline=Date.now()+5000;let idle=0;
    while(Date.now()<deadline) {
      if(String(performance.timeOrigin)!==arg.document_id)throw Error('Attachment document changed during cleanup');
      if(hasAttachments(form()))idle=0;else if(++idle===2)return true;
      await settle(100);
    }
    throw Error('Attachment cleanup not confirmed');
  }
  if(op==='attachments') {
    const f=form();return arg.every(name=>all('[role="group"][aria-label], .composer-attachment-surface:is(button,[role="button"])[aria-label]',f).some(e=>visible(e)&&e.getAttribute('aria-label')===name)) && !all('[role="progressbar"], [data-state="uploading"]',f).some(visible);
  }
  if(op==='snapshot') {
    const groups=all('[data-turn-key]');const containers=all('[data-turn-id-container]').filter(e=>!e.closest('[data-turn-key]')&&e.parentElement?.closest('[data-turn-id-container]')?.getAttribute('data-turn-id-container')!==e.getAttribute('data-turn-id-container'));
    const ids=containers.map(e=>e.getAttribute('data-turn-id-container'));const users=[],responses=[];
    const legacy=(role)=>all('[data-testid^="conversation-turn-"]').filter(e=>!e.closest('[data-turn-key]')&&(e.getAttribute('data-turn')===role||e.getAttribute('data-message-author-role')===role||e.querySelector('[data-message-author-role="'+role+'"]')));
    for(const role of ['user','assistant'])for(const e of legacy(role)){const id=e.getAttribute('data-turn-id');if(!id||!ids.includes(id))throw Error('Missing logical turn container');(role==='user'?users:responses).push({id,...(role==='user'?messageText(e):{}),root:e});}
    for(const g of groups){const key=g.getAttribute('data-turn-key');if(!key)throw Error('Missing logical group key');ids.push('group:user:'+key,'group:assistant:'+key);const u=g.querySelector('[data-user-message-bubble]');if(u)users.push({id:'group:user:'+key,...messageText(u),root:u});if(g.querySelector('[data-conversation-role="assistant"], [data-chatgpt-agent-turn-start]'))responses.push({id:'group:assistant:'+key,root:g});}
    if(new Set(ids).size!==ids.length)throw Error('Duplicate logical turn identity');
    const running=all(stopSelector).some(visible);
    return {document_id:String(performance.timeOrigin),ids,users:users.map(({id,text,tokens,pending})=>({id,text,tokens,pending})),responses:responses.map(({id,root})=>{
      const selector='.markdown, [data-message-author-role="assistant"] .puik-root.not-markdown > [class*="_DilResponseRoot"], [data-markdown-text-style="assistant-message"]';
      const activities=all('[data-chatgpt-agent-turn-start]',root).map(e=>e.parentElement);
      const markdown=all(selector,root).filter(rendered).filter(e=>!e.parentElement?.closest(selector)).filter(e=>{
        if(!root.hasAttribute('data-turn-key')&&!e.hasAttribute('data-markdown-text-style'))return true;
        const unit=e.closest('[data-content-search-unit-key]');return unit?[...unit.children].some(c=>c.getAttribute('data-conversation-role')==='assistant'):activities.some(a=>a.contains(e));
      });
      const classified=selectChatGptAnswerRoots(markdown,all('[data-streaming-response-status]',root).filter(rendered),activities);
      const blocks=classified.answerRoots;
      const last=blocks.at(-1);const controls=all('button[data-testid="copy-turn-action-button"], .turn-action-controls button',root).filter(rendered);
      return {id,text:blocks.map(e=>e.innerText).join('\n\n'),html:blocks.map(e=>e.outerHTML).join(''),commentary:classified.commentaryRoots.map(e=>e.innerText).join('\n\n'),complete:!!last&&controls.some(e=>!!(last.compareDocumentPosition(e)&Node.DOCUMENT_POSITION_FOLLOWING))&&!running};
    }),running};
  }
  throw Error('Unknown page operation');
}
