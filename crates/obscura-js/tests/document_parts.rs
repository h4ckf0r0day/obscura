use obscura_js::runtime::ObscuraJsRuntime;

#[test]
fn document_root_accessors_use_native_structure_and_receiver_realm() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<html><head></head><body></body></html>"));
    rt.set_url("http://example.com/test");
    rt.run_page_init();
    let result = rt.evaluate(r#"(() => {
 const htmlNS='http://www.w3.org/1999/xhtml',svgNS='http://www.w3.org/2000/svg';
 const result=[];
 for(const [name,doc]of[['connected',document],['detached',document.implementation.createHTMLDocument('owned')]]){
  const row={name},root=doc.documentElement,head=doc.head,body=doc.body;
  const descriptor=Object.getOwnPropertyDescriptor(doc,'querySelector');
  Object.defineProperty(doc,'querySelector',{value:()=>null,configurable:true});
  row.overrideIgnored=[doc.head===head,doc.body===body];
  if(descriptor)Object.defineProperty(doc,'querySelector',descriptor);else delete doc.querySelector;
  const fakeHead=doc.createElementNS(svgNS,'head'),fakeBody=doc.createElementNS(svgNS,'body');
  root.insertBefore(fakeHead,head);root.insertBefore(fakeBody,head);
  row.foreignNamesIgnored=[doc.head===head,doc.body===body];fakeHead.remove();fakeBody.remove();
  const frameset=doc.createElement('frameset');root.insertBefore(frameset,body);
  row.framesetIsBody=doc.body===frameset;frameset.remove();
  const wrapper=doc.createElement('div');root.appendChild(wrapper);wrapper.appendChild(body);
  row.nestedBodyExcluded=doc.body===null;root.appendChild(body);wrapper.remove();
  if(name==='connected'){
   const svg=doc.createElementNS(svgNS,'svg');doc.replaceChild(svg,root);
   row.svgDocumentElement=doc.documentElement===svg;
   doc.replaceChild(root,svg);
  }
  result.push(row);
 }
 const getters=Object.fromEntries(['documentElement','head','body'].map(name=>[name,Object.getOwnPropertyDescriptor(Document.prototype,name).get]));
 const brands={};
 for(const [name,get]of Object.entries(getters)){
  brands[name]=[null,{},Object.create(Document.prototype),document.body,new Proxy(document,{})].every(value=>{try{get.call(value);return false}catch(error){return error instanceof TypeError}});
 }
 result.push({name:'brands',...brands});
 const frame=document.createElement('iframe');document.body.appendChild(frame);
 const child=frame.contentDocument,foreign={name:'crossRealm'};
 for(const name of ['documentElement','head','body']){
  try{
   const expected=child[name],childGet=Object.getOwnPropertyDescriptor(frame.contentWindow.Document.prototype,name).get;
   foreign[name]=getters[name].call(child)===expected&&childGet.call(document)===document[name];
  }catch(error){foreign[name]=false;}
 }
 result.push(foreign);frame.remove();
 const extra={name:'constructedAndMutated'};
 const xml=document.implementation.createDocument(htmlNS,'html',null),xmlRoot=xml.documentElement;
 const wrapper=xml.createElementNS(htmlNS,'wrapper');
 for(const tag of ['head','body']){const node=xml.createElementNS(htmlNS,tag);node.id='nested';wrapper.appendChild(node)}
 xmlRoot.appendChild(wrapper);
 for(const tag of ['head','body']){const node=xml.createElementNS(htmlNS,tag);node.id='direct-'+tag;xmlRoot.appendChild(node)}
 extra.xmlDirect=xml.head?.id==='direct-head'&&xml.body?.id==='direct-body'&&getters.head.call(xml)===xml.head&&getters.body.call(xml)===xml.body;
 const svgDoc=document.implementation.createDocument(svgNS,'svg',null);
 const svgBody=svgDoc.createElementNS(htmlNS,'body');svgDoc.documentElement.appendChild(svgBody);
 extra.svgExcluded=svgDoc.head===null&&svgDoc.body===null&&getters.documentElement.call(svgDoc)===svgDoc.documentElement;
 const empty=new Document();extra.empty=empty.documentElement===null&&empty.head===null&&empty.body===null;
 const replaced=document.implementation.createHTMLDocument('replace'),oldRoot=replaced.documentElement;
 replaced.removeChild(oldRoot);extra.removed=replaced.documentElement===null&&replaced.head===null&&replaced.body===null;
 const newRoot=replaced.createElement('html'),newHead=replaced.createElement('head'),newBody=replaced.createElement('body');newRoot.append(newHead,newBody);replaced.appendChild(newRoot);
 extra.replaced=replaced.documentElement===newRoot&&replaced.head===newHead&&replaced.body===newBody;
 replaced._nid=document._nid;
 extra.authorDataIgnored=getters.documentElement.call(replaced)===newRoot&&getters.head.call(replaced)===newHead&&getters.body.call(replaced)===newBody;
 const errorDoc=new DOMParser().parseFromString('<broken>','application/xml');
 errorDoc.removeChild(errorDoc.documentElement);
 extra.errorRootRemoved=errorDoc.documentElement===null&&getters.documentElement.call(errorDoc)===null&&errorDoc.head===null&&errorDoc.body===null;
 const replacement=errorDoc.createElement('replacement');errorDoc.appendChild(replacement);
 extra.errorRootReplaced=errorDoc.documentElement===replacement&&getters.documentElement.call(errorDoc)===replacement&&errorDoc.head===null&&errorDoc.body===null;
 result.push(extra);
 return result;
})()"#).unwrap();
    assert_eq!(result, serde_json::json!([{"name":"connected","overrideIgnored":[true,true],"foreignNamesIgnored":[true,true],"framesetIsBody":true,"nestedBodyExcluded":true,"svgDocumentElement":true},{"name":"detached","overrideIgnored":[true,true],"foreignNamesIgnored":[true,true],"framesetIsBody":true,"nestedBodyExcluded":true},{"name":"brands","documentElement":true,"head":true,"body":true},{"name":"crossRealm","documentElement":true,"head":true,"body":true},{"name":"constructedAndMutated","xmlDirect":true,"svgExcluded":true,"empty":true,"removed":true,"replaced":true,"authorDataIgnored":true,"errorRootRemoved":true,"errorRootReplaced":true}]));
}

#[test]
fn xml_error_documents_use_native_nodes_and_ignore_author_hooks() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<html><head></head><body></body></html>"));
    rt.set_url("http://example.com/parser");
    rt.run_page_init();
    let result = rt.evaluate(r#"(() => {
        const NativeXMLDocument = XMLDocument;
        const parser = new DOMParser(), doc = parser.parseFromString('<broken>', 'application/xml');
        const root = doc.documentElement;
        const checks = {
            document: doc instanceof NativeXMLDocument && root.parentNode === doc && root.ownerDocument === doc,
            namespace: root.namespaceURI === 'http://www.mozilla.org/newlayout/xml/parsererror.xml',
            metadata: doc.contentType === 'application/xml' && doc.URL === document.URL,
        };
        const originalConstructor = globalThis.XMLDocument;
        const originalElement = Document.prototype.createElementNS;
        const originalText = Document.prototype.createTextNode;
        const originalAppend = Node.prototype.appendChild;
        let calls = 0;
        const blocked = () => { calls++; throw new Error('author hook'); };
        try {
            globalThis.XMLDocument = blocked;
            Document.prototype.createElementNS = blocked;
            Document.prototype.createTextNode = blocked;
            Node.prototype.appendChild = blocked;
            const parsed = parser.parseFromString('<broken>', 'application/xml');
            checks.hooks = calls === 0 && parsed instanceof NativeXMLDocument && parsed.documentElement.parentNode === parsed;
        } catch (_) { checks.hooks = false; }
        finally {
            globalThis.XMLDocument = originalConstructor;
            Document.prototype.createElementNS = originalElement;
            Document.prototype.createTextNode = originalText;
            Node.prototype.appendChild = originalAppend;
        }
        return checks;
    })()"#).unwrap();
    assert_eq!(result, serde_json::json!({"document":true,"namespace":true,"metadata":true,"hooks":true}));
}

#[test]
fn document_id_lookup_rejects_non_descendant_index_entries() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<html><body><div id='live'>original</div></body></html>"));
    rt.set_url("https://example.test/");
    rt.run_page_init();
    rt.with_dom(|dom| dom.update_id_index(dom.document(), None, Some("document-id"))).unwrap();
    let result = rt.evaluate(r#"(() => {
        const live = document.getElementById('live');
        const detached = live.cloneNode(true);
        const detachedIndex = document.getElementById('live') === live;
        const host = document.createElement('section');
        document.body.appendChild(host);
        host.attachShadow({mode:'open'}).appendChild(detached);
        const shadowIndex = document.getElementById('live') === live;
        live.remove();
        return [document.getElementById('document-id') === null,
            detachedIndex, shadowIndex, document.getElementById('live') === null];
    })()"#).unwrap();
    assert_eq!(result, serde_json::json!([true,true,true,true]));
}
