const readline = require('readline');
const { startServer } = require('./server');

async function main() {
  const api = process.env.CLANKBAN_API_URL ? {url:process.env.CLANKBAN_API_URL} : await startServer({port:0, dataDir:process.env.CLANKBAN_DATA_DIR});
  const tools = [
    {name:'get_agent_context',description:'Get a compact summary of active Clankban work for planning or delegation.',inputSchema:{type:'object',properties:{status:{type:'string',description:'Optional status filter'}}}},
    {name:'get_task',description:'Read one work item by its stable ID.',inputSchema:{type:'object',properties:{id:{type:'string'}},required:['id']}},
    {name:'create_task',description:'Create an epic, task, subtask, or bug fix in the local board. Use status backlog, progress, review, or done, include parentId for child tasks, and include subtasks when known.',inputSchema:{type:'object',properties:{title:{type:'string'},description:{type:'string'},type:{type:'string'},priority:{type:'string'},status:{type:'string',enum:['backlog','progress','review','done']},due:{type:'string'},parentId:{type:'string'},subtasks:{type:'array',items:{type:'object',properties:{title:{type:'string'},done:{type:'boolean'}},required:['title']}}},required:['title']}},
    {name:'update_task',description:'Update a local work item by ID, including its parent relationship.',inputSchema:{type:'object',properties:{id:{type:'string'},title:{type:'string'},description:{type:'string'},priority:{type:'string'},status:{type:'string'},due:{type:'string'},parentId:{type:'string'}},required:['id']}}
  ];
  const send = value => process.stdout.write(JSON.stringify(value)+'\n');
  const rl=readline.createInterface({input:process.stdin,crlfDelay:Infinity});
  for await (const line of rl) { let msg; try{msg=JSON.parse(line)}catch{continue} if(msg.method==='initialize'){send({jsonrpc:'2.0',id:msg.id,result:{protocolVersion:'2025-06-18',capabilities:{tools:{}},serverInfo:{name:'clankban',version:'0.1.0'}}});continue} if(msg.method==='tools/list'){send({jsonrpc:'2.0',id:msg.id,result:{tools}});continue} if(msg.method!=='tools/call')continue; let result; try { const args=msg.params.arguments||{}; if(msg.params.name==='get_agent_context'){result=await fetch(`${api.url}/api/agent-context`).then(r=>r.json()); if(args.status)result.tasks=result.tasks.filter(t=>t.status===args.status)} else if(msg.params.name==='get_task') result=await fetch(`${api.url}/api/tasks/${encodeURIComponent(args.id)}`).then(r=>r.json()); else if(msg.params.name==='create_task') result=await fetch(`${api.url}/api/tasks`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(args)}).then(r=>r.json()); else if(msg.params.name==='update_task') {const {id,...patch}=args; result=await fetch(`${api.url}/api/tasks/${encodeURIComponent(id)}`,{method:'PATCH',headers:{'content-type':'application/json'},body:JSON.stringify(patch)}).then(r=>r.json())} else throw new Error('Unknown tool'); send({jsonrpc:'2.0',id:msg.id,result:{content:[{type:'text',text:JSON.stringify(result)}]}})}catch(e){send({jsonrpc:'2.0',id:msg.id,error:{code:-32000,message:e.message}})} }
}
main();
