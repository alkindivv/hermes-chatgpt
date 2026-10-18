import Cocoa
import WebKit

// Disposable WebKit DOM fixtures. This does not load ChatGPT or an existing browser profile.
final class FixtureRunner: NSObject, WKNavigationDelegate {
    let browser: WKWebView
    let window: NSWindow
    let source: String
    init(source: String) {
        self.source = source
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        browser = WKWebView(frame: NSRect(x: 0, y: 0, width: 900, height: 700), configuration: config)
        window = NSWindow(contentRect: browser.frame, styleMask: [.titled], backing: .buffered, defer: false)
        super.init()
        window.contentView = browser
        browser.navigationDelegate = self
    }
    func start() {
        browser.loadHTMLString("<html><head><style>body{font:16px sans-serif}button,[contenteditable]{display:block;min-height:25px} [hidden]{display:none}</style></head><body></body></html>", baseURL: nil)
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        // Inject only the fixture origin: these cases test real DOM layout/selection and serialization,
        // while tools/test-auth.cjs and Rust endpoint tests exercise the production origin checks.
        let test = #"""
        const page = new Function('location', source+'; return page;')({origin:'https://chatgpt.com'});
        const failures=[];let passed=0;
        const assert=(condition,message)=>{if(!condition)throw Error(message);};
        const run=async(name,fn)=>{try{await fn();passed++;}catch(e){failures.push(name+': '+e.message);}};
        const editor=html=>{document.body.innerHTML='<form><div id="prompt-textarea" contenteditable="true">'+html+'</div></form>';};
        const pill='<span contenteditable="false" app-mention-path="app://fixture" app-mention-display-name="Codex Native2">Codex Native2</span>';
        await run('paragraph line breaks exclude the connector',async()=>{
          editor('<p>'+pill+'first<br>second</p>');
          const value=await page('connectorSnapshot',{name:'Codex Native2'});
          assert(value.text==='first\nsecond','line break was lost or connector leaked into the prompt');
          assert(value.selected.length===1&&value.selected[0].path==='app://fixture','app mention identity missing');
          assert(await page('verifyPrompt','first\nsecond')===true,'full prompt differs');
        });
        await run('legacy plugin identity and empty input',async()=>{
          editor('<p><span data-id="plugin:fixture" data-keyword="Codex Native2" contenteditable="false">Codex Native2</span></p>');
          const value=await page('connectorSnapshot',{name:'Codex Native2'});
          assert(value.text===''&&value.selected.length===1&&value.selected[0].name==='Codex Native2','legacy selection mismatch');
          assert(await page('focus',null)===true,'selected pill was mistaken for a user draft');
        });
        await run('paragraphs retain complete text',async()=>{
          editor('<p>first</p><p>second</p>');
          assert((await page('connectorSnapshot',{name:'Codex Native2'})).text==='first\nsecond','paragraphs changed');
        });
        await run('mention rows preserve exact names and exclude hidden duplicates',async()=>{
          editor('<p>@codex</p>');
          document.body.insertAdjacentHTML('beforeend','<div data-mention-list-scroll-area><button data-list-navigation-item="true"><span>Codex Other</span></button><button data-list-navigation-item="true" aria-current="true"><span>Codex Native2</span><small>description</small></button><button data-list-navigation-item="true" hidden><span>Codex Native2</span></button></div>');
          const value=await page('connectorSnapshot',{name:'Codex Native2'});
          assert(value.rows.length===2,'hidden row counted');
          assert(value.rows[1].name==='Codex Native2'&&value.rows[1].highlighted===true,'exact keyboard row not recognized');
        });
        await run('visible duplicates remain visible to the Rust rejection policy',async()=>{
          editor('<p>@codex</p>');
          document.body.insertAdjacentHTML('beforeend','<div class="__menu-item" tabindex="0">Codex Native2</div><div class="__menu-item" tabindex="0">Codex Native2</div>');
          assert((await page('connectorSnapshot',{name:'Codex Native2'})).rows.filter(row=>row.name==='Codex Native2').length===2,'duplicate exact name lost');
        });
        await run('focus refuses a different document',async()=>{
          editor('<p></p>');let rejected=false;
          try{await page('connectorFocus',{document_id:'different-document'});}catch{rejected=true;}
          assert(rejected,'a stale document was allowed to receive input');
        });
        const readyToSend=html=>{
          editor(html);const button=document.createElement('button');button.type='button';button.dataset.testid='send-button';
          let clicks=0;button.addEventListener('click',()=>clicks++);document.querySelector('form').append(button);
          return ()=>clicks;
        };
        const sendArgs=()=>({document_id:String(performance.timeOrigin),full_prompt:'first\nsecond',connector_name:'Codex Native2',connector_path:'app://fixture'});
        await run('send accepts the complete prompt and exact connector',async()=>{
          const clicks=readyToSend('<p>'+pill+'first</p><p>second</p>');
          assert(await page('send',sendArgs())===true,'valid Send refused');
          assert(clicks()===1,'valid Send must click exactly once');
        });
        for(const [name,mutate] of [
          ['edited prompt',()=>document.querySelector('p').append(' changed')],
          ['removed connector',()=>document.querySelector('[app-mention-path]').remove()],
          ['swapped connector',()=>document.querySelector('[app-mention-path]').setAttribute('app-mention-display-name','Other')],
          ['swapped same-name connector',()=>document.querySelector('[app-mention-path]').setAttribute('app-mention-path','app://different')],
          ['duplicate connector',()=>document.querySelector('p').insertAdjacentHTML('afterbegin',pill)],
          ['document changed',args=>args.document_id='previous-document'],
          ['unrequested connector',args=>args.connector_name=null],
        ]) await run('send refuses '+name+' after preparation',async()=>{
          const clicks=readyToSend('<p>'+pill+'first</p><p>second</p>');
          await page('verifyPrompt','first\nsecond');const args=sendArgs();mutate(args);
          let rejected=false;try{await page('send',args);}catch{rejected=true;}
          assert(rejected&&clicks()===0,'integrity failure clicked Send');
        });
        await run('send without a connector requires no selection',async()=>{
          const clicks=readyToSend('<p>first</p><p>second</p>');const args=sendArgs();args.connector_name=null;args.connector_path=null;
          assert(await page('send',args)===true&&clicks()===1,'plain prompt refused');
        });
        const attachment=name=>{
          const tile=document.createElement('div');tile.setAttribute('role','group');tile.setAttribute('aria-label',name);
          const remove=document.createElement('button');remove.type='button';remove.setAttribute('aria-label','Remove file');
          remove.addEventListener('click',()=>tile.remove());tile.append(remove);document.querySelector('form').append(tile);return tile;
        };
        await run('preparation snapshot reports existing attachments',async()=>{
          editor('<p></p>');attachment('user.txt');
          assert((await page('connectorSnapshot',{name:'Codex Native2'})).has_attachments===true,'existing attachment was invisible to preflight');
        });
        await run('rollback removes only the owned named attachment',async()=>{
          editor('<p>owned draft</p>');const tile=attachment('owned.txt');
          assert(await page('clearAttachments',{document_id:String(performance.timeOrigin),names:['owned.txt']})===true,'cleanup did not finish');
          assert(!tile.isConnected,'owned file tile remains');
        });
        await run('rollback preserves unknown attachments',async()=>{
          editor('<p></p>');const owned=attachment('owned.txt'),user=attachment('user.txt');let rejected=false;
          try{await page('clearAttachments',{document_id:String(performance.timeOrigin),names:['owned.txt']});}catch{rejected=true;}
          assert(rejected&&owned.isConnected&&user.isConnected,'cleanup altered an unowned attachment set');
        });
        await run('rollback refuses an unrecognized remove control',async()=>{
          editor('<p></p>');const tile=attachment('owned.txt');tile.querySelector('button').setAttribute('aria-label','Open file');let rejected=false;
          try{await page('clearAttachments',{document_id:String(performance.timeOrigin),names:['owned.txt']});}catch{rejected=true;}
          assert(rejected&&tile.isConnected,'cleanup clicked a guessed control');
        });
        return {passed,failures};
        """#
        webView.callAsyncJavaScript(test, arguments: ["source": source], in: nil, in: .page) { result in
            switch result {
            case .success(let value):
                guard let report = value as? [String: Any], let failures = report["failures"] as? [String] else {
                    fputs("DOM fixture returned invalid output\n", stderr); exit(1)
                }
                print("connector DOM: \(report["passed"] ?? 0) passed; \(failures.count) failed")
                for failure in failures { print(failure) }
                exit(failures.isEmpty ? 0 : 1)
            case .failure(let error):
                fputs("DOM fixture failed: \(error.localizedDescription)\n", stderr); exit(1)
            }
        }
    }
}

guard CommandLine.arguments.count == 2 else { fputs("Usage: test-connector-dom.swift PAGE_JS\n", stderr); exit(2) }
let application = NSApplication.shared
application.setActivationPolicy(.prohibited)
let runner = FixtureRunner(source: try String(contentsOfFile: CommandLine.arguments[1], encoding: .utf8))
runner.start()
DispatchQueue.main.asyncAfter(deadline: .now() + 25) { fputs("DOM fixture timed out\n", stderr); exit(1) }
application.run()
