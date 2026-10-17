// The generator inserts the original Node::execute / Node::validate definitions
// at MMX_NATIVE_VALIDATION below. Only storage, indexes and locking are adapted.
#include <mmx/Transaction.hxx>
#include <mmx/BlockHeader.hxx>
#include <mmx/contract/PubKey.hxx>
#include <mmx/contract/MultiSig.hxx>
#include <mmx/contract/Executable.hxx>
#include <mmx/contract/Binary.hxx>
#include <mmx/contract/Data.hxx>
#include <mmx/operation/Deposit.hxx>
#include <mmx/solution/PubKey.hxx>
#include <mmx/vm_interface.h>
#include <mmx/vm/StorageRAM.h>
#include <mmx/vm/StorageCache.h>
#include <mmx/utils.h>
#include <mmx/exception.h>
#include <mmx/error_code_e.hxx>
#include <mmx/txio_t.hpp>
#include <vnx/vnx.h>
#include <fstream>
#include <iomanip>
#include <sstream>
namespace mmx {
using Balances=std::map<std::pair<addr_t,addr_t>,uint128>;
struct balance_cache_t {
    const Balances* source;Balances balance;
    balance_cache_t(const Balances* s):source(s){}
    uint128* find(addr_t a,addr_t c){auto key=std::make_pair(a,c);auto it=balance.find(key);if(it!=balance.end())return &it->second;auto v=source->find(key);return v==source->end()?nullptr:&balance.emplace(key,v->second).first->second;}
};
struct ReferenceNode {
    struct execution_context_t {bool do_profile=false,do_trace=false;uint32_t height=0;std::shared_ptr<vm::StorageCache> storage;std::unordered_map<addr_t,std::vector<hash_t>> mutate_map;};
    std::shared_ptr<ChainParams> params;
    Balances balance_table;
    std::map<addr_t,tx_index_t> tx_index;
    std::map<addr_t,std::shared_ptr<Contract>> contracts;
    bool exec_debug=false;
    std::shared_ptr<const Contract> get_contract_ex(const addr_t& a,uint64_t* cost=nullptr,uint64_t limit=UINT64_MAX)const{
        if(cost){auto i=tx_index.find(a);const uint64_t charge=params->min_txfee_read+(i==tx_index.end()?0:i->second.contract_read_cost);if(*cost+charge>limit)throw std::runtime_error("not enough gas to read contract");*cost+=charge;}
        auto i=contracts.find(a);return i==contracts.end()?nullptr:i->second;
    }
    std::shared_ptr<const Contract> get_contract_for_ex(const addr_t& a,uint64_t* cost=nullptr,uint64_t limit=UINT64_MAX)const{if(auto c=get_contract_ex(a,cost,limit))return c;auto p=contract::PubKey::create();p->address=a;return p;}
    template<class T>std::shared_ptr<const T> get_contract_as(const addr_t& a,uint64_t* cost=nullptr,uint64_t limit=UINT64_MAX)const{return std::dynamic_pointer_cast<const T>(get_contract_ex(a,cost,limit));}
    void execute(std::shared_ptr<const Transaction>,std::shared_ptr<const execution_context_t>,std::shared_ptr<const operation::Execute>,std::shared_ptr<const Contract>,const addr_t&,std::vector<txout_t>&,std::map<std::pair<addr_t,addr_t>,uint128>&,std::shared_ptr<vm::StorageCache>,uint64_t&,exec_error_t&,bool)const;
    void execute(std::shared_ptr<const Transaction>,std::shared_ptr<const execution_context_t>,std::shared_ptr<const contract::Executable>,std::vector<txout_t>&,std::map<std::pair<addr_t,addr_t>,uint128>&,std::shared_ptr<vm::StorageCache>,std::shared_ptr<vm::Engine>,const std::string&,exec_error_t&,bool,const std::function<void(std::shared_ptr<vm::Engine>)>&)const;
    std::shared_ptr<const exec_result_t> validate(std::shared_ptr<const Transaction>,std::shared_ptr<const execution_context_t>)const;
};
// MMX_NATIVE_VALIDATION
}
using namespace mmx;using namespace mmx::vm;
std::string hex(const uint8_t* data,size_t size){std::ostringstream s;s<<std::hex<<std::setfill('0');for(size_t i=0;i<size;++i)s<<std::setw(2)<<unsigned(data[i]);return s.str();}
template<size_t N>std::string hx(const bytes_t<N>& value){return hex(value.data(),N);}
std::string wire(std::shared_ptr<const vnx::Value> value){std::vector<uint8_t>b;vnx::VectorOutputStream s(&b);vnx::TypeOutput out(&s);vnx::write(out,value);out.flush();return hex(b.data(),b.size());}
std::string result_wire(const exec_result_t& v){std::vector<uint8_t>b;vnx::VectorOutputStream s(&b);vnx::TypeOutput out(&s);vnx::write(out,v,nullptr,nullptr);out.flush();return hex(b.data(),b.size());}
addr_t addr(uint64_t n){addr_t a;a.from_uint(n);return a;}
skey_t secret(){skey_t s;s[31]=1;return s;}
addr_t payer(){return pubkey_t(secret()).get_addr();}
void seal(std::shared_ptr<Transaction> tx,std::shared_ptr<ChainParams> p){tx->id=tx->calc_hash();auto sol=solution::PubKey::create();sol->pubkey=pubkey_t(secret());sol->signature=signature_t::sign(secret(),tx->id);if(tx->solutions.empty())tx->solutions={sol};else tx->solutions[0]=sol;tx->static_cost=tx->calc_cost(p);tx->content_hash=tx->calc_hash(true);}
struct Recorder:StorageRAM{
    struct Cell{addr_t c;uint64_t a;vnx::optional<uint64_t> k;std::shared_ptr<var_t> v;};std::vector<Cell>writes;
    void write(const addr_t&c,uint64_t a,const var_t&v)override{StorageRAM::write(c,a,v);writes.push_back({c,a,nullptr,std::shared_ptr<var_t>(clone(v))});}
    void write(const addr_t&c,uint64_t a,uint64_t k,const var_t&v)override{StorageRAM::write(c,a,k,v);writes.push_back({c,a,k,std::shared_ptr<var_t>(clone(v))});}
};
struct Spec{std::string name;ReferenceNode node;std::shared_ptr<Transaction> tx;uint32_t height=10;unsigned corruption=0;};
auto binary(std::vector<instr_t> code){auto b=contract::Binary::create();std::vector<std::unique_ptr<var_t>>v;v.push_back(std::make_unique<var_t>());v.push_back(std::make_unique<uint_t>(1));v.push_back(to_binary(std::string("dep")));v.push_back(to_binary(std::string("run")));v.push_back(to_binary(addr(9)));v.push_back(std::make_unique<uint_t>(7));v.push_back(to_binary(addr_t()));v.push_back(to_binary(std::string("memo")));v.push_back(to_binary(std::string("failure")));for(const auto&x:v){const auto d=serialize(*x,false,false);b->constant.insert(b->constant.end(),d.first.get(),d.first.get()+d.second);}b->binary=serialize(code);contract::method_t m;m.name="run";m.is_public=true;b->methods["run"]=m;m.name="init";m.is_public=false;m.is_init=true;b->methods["init"]=m;b->line_info[0]=11;return b;}
Spec base(std::string name,unsigned v){Spec s;s.name=name;s.height=v?10:9;s.node.params=ChainParams::create();s.node.params->network="mainnet";s.node.params->hardfork2_height=10;s.tx=Transaction::create();s.tx->version=v;s.tx->network="mainnet";s.tx->nonce=1;s.tx->max_fee_amount=1000000;s.tx->sender=payer();s.node.balance_table[{payer(),addr(0)}]=2000000;
    for(unsigned id:{1,2}){auto e=contract::Executable::create();e->binary=addr(100+id);if(id==1)e->depends["dep"]=addr(2);s.node.contracts[addr(id)]=e;s.node.contracts[e->binary]=binary({{OP_RET}});s.node.tx_index[addr(id)].contract_read_cost=10;s.node.tx_index[e->binary].contract_read_cost=20;s.node.balance_table[{addr(id),addr(0)}]=100;}
    seal(s.tx,s.node.params);return s;}
void input(Spec&s,uint128 n,addr_t currency=addr(0)){txin_t in;in.address=payer();in.contract=currency;in.amount=n;in.solution=0;s.tx->inputs.push_back(in);}
void output(Spec&s,uint128 n,addr_t currency=addr(0)){txout_t out;out.address=addr(9);out.contract=currency;out.amount=n;s.tx->outputs.push_back(out);}
void call(Spec&s,std::string method="run",bool user=false){auto op=operation::Execute::create();op->address=addr(1);op->method=method;if(user){op->user=payer();op->solution=0;}s.tx->execute.push_back(op);}
void code(Spec&s,std::vector<instr_t> instructions){s.node.contracts[addr(101)]=binary(instructions);}
std::vector<Spec> cases(){std::vector<Spec>out;for(unsigned v:{0,1}){
    auto add=[&](Spec s){s.name+="_v"+std::to_string(v);seal(s.tx,s.node.params);
        if(s.corruption){const size_t i=s.corruption<=2?0:1;auto sol=solution::PubKey::create();sol->pubkey=pubkey_t(secret());
            if(s.corruption==2||s.corruption==4)sol->pubkey=pubkey_t();
            if(s.corruption==5){const bytes_t<32> n("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141");for(unsigned j=0;j<32;++j)sol->signature[j]=n[j];}
            if(s.corruption==6){skey_t other;other[31]=2;sol->pubkey=pubkey_t(other);sol->signature=signature_t::sign(other,s.tx->id);}
            s.tx->solutions[i]=sol;s.tx->static_cost=s.tx->calc_cost(s.node.params);s.tx->content_hash=s.tx->calc_hash(true);
        }
        out.push_back(s);};
    add(base("empty",v));auto s=base("transfer",v);input(s,100);output(s,100);add(s);
    s=base("zero_output",v);output(s,0);add(s);s=base("overspend",v);input(s,1);output(s,2);add(s);s=base("leftover",v);input(s,1);add(s);
    s=base("insufficient_input",v);input(s,2000000);add(s);s=base("missing_balance",v);input(s,0,addr(77));add(s);
    s=base("expired",v);s.tx->expires=s.height-1;add(s);s=base("expires_at_height",v);s.tx->expires=s.height;add(s);
    s=base("fee_missing",v);s.node.balance_table.erase({payer(),addr(0)});add(s);s=base("fee_short",v);s.node.balance_table[{payer(),addr(0)}]=1099;add(s);
    for(unsigned cap:{1099,1100,1101}){s=base("fee_cap_"+std::to_string(cap),v);s.tx->max_fee_amount=cap;add(s);}
    s=base("wrong_version",v);s.tx->version=1-v;add(s);s=base("duplicate",v);s.node.tx_index[s.tx->id];add(s);
    s=base("call",v);call(s);add(s);s=base("user_call",v);call(s,"run",true);add(s);s=base("missing_method",v);call(s,"missing");add(s);
    s=base("write",v);code(s,{{OP_COPY,0,MEM_STATIC,1},{OP_RET}});call(s);add(s);
    s=base("write_then_fail",v);code(s,{{OP_COPY,0,MEM_STATIC,1},{OP_FAIL,0,8,19}});call(s);add(s);
    s=base("send",v);code(s,{{OP_SEND,0,4,5,6,7},{OP_RET}});call(s);add(s);
    s=base("mint",v);code(s,{{OP_MINT,0,4,5,7},{OP_RET}});call(s);add(s);
    s=base("send_then_fail",v);code(s,{{OP_SEND,0,4,5,6,7},{OP_FAIL,0,8,19}});call(s);add(s);
    s=base("child",v);code(s,{{OP_RCALL,0,2,3,0,0},{OP_RET}});s.node.contracts[addr(102)]=binary({{OP_COPY,0,MEM_STATIC,1},{OP_SEND,0,4,5,6,0},{OP_RET}});call(s);add(s);
    s=base("child_parent_fail",v);code(s,{{OP_RCALL,0,2,3,0,0},{OP_FAIL,0,8,19}});s.node.contracts[addr(102)]=binary({{OP_COPY,0,MEM_STATIC,1},{OP_RET}});call(s);add(s);
    s=base("multi_op_rollback",v);code(s,{{OP_COPY,0,MEM_STATIC,1},{OP_RET}});call(s);call(s,"missing");add(s);
    s=base("invalid_deploy",v);s.tx->deploy=contract::PubKey::create();add(s);
    s=base("deploy_data",v);auto d=contract::Data::create();d->value=std::string({char(255),char(0)});s.tx->deploy=d;add(s);
    s=base("implicit",v);s.tx->deploy=d;input(s,100);add(s);
    for(unsigned n:{2,13,14,29,30,59,60}){s=base("implicit_many_"+std::to_string(n),v);s.tx->deploy=d;for(unsigned i=0;i<n;++i){const auto cur=addr((i*13)%79+1000);s.node.balance_table[{payer(),cur}]=100;input(s,i+1,cur);}add(s);}
    s=base("constructor",v);auto e=contract::Executable::create();e->binary=addr(101);s.tx->deploy=e;code(s,{{OP_COPY,0,MEM_STATIC,1},{OP_RET}});add(s);
    s=base("constructor_failure",v);s.tx->deploy=e;code(s,{{OP_FAIL,0,8,71}});add(s);
    s=base("deployment_missing_dependency",v);auto dep=std::make_shared<contract::Executable>(*e);dep->depends["gone"]=addr(199);s.tx->deploy=dep;add(s);
    for(unsigned n:{1,7,20}){s=base("deposit_"+std::to_string(n),v);input(s,n);auto op=operation::Deposit::create();op->address=addr(1);op->method="run";op->amount=n;s.tx->execute={op};code(s,{{OP_SEND,0,4,5,6,0},{OP_RET}});add(s);}
    s=base("deposit_overspend",v);input(s,1);auto op=operation::Deposit::create();op->address=addr(1);op->method="run";op->amount=2;s.tx->execute={op};add(s);
    s=base("explicit_vm_overlap",v);s.tx->sender=payer();auto ms=contract::MultiSig::create();ms->owners={payer()};ms->num_required=1;s.node.contracts[addr(3)]=ms;s.node.tx_index[addr(3)].contract_read_cost=12;s.node.balance_table[{addr(3),addr(0)}]=100;txin_t in;in.address=addr(3);in.amount=99;in.solution=0;in.flags=1;s.tx->inputs={in};output(s,99);add(s);
    s=base("dynamic_fee_uses_reserved_balance",v);input(s,1988500);output(s,1988500);call(s);code(s,{{OP_FAIL,0,8,19}});add(s);
    for(unsigned ratio:{1023,1024,1025,2048,102400}){s=base("ratio_"+std::to_string(ratio),v);call(s);s.tx->fee_ratio=ratio;add(s);}
    for(unsigned cap:{11100,12109,12110,13000,15000,20000,50000}){s=base("gas_"+std::to_string(cap),v);call(s);s.tx->max_fee_amount=cap;code(s,{{OP_COPY,0,MEM_STATIC,1},{OP_RET}});add(s);}
    s=base("max_cost",v);s.node.params->max_tx_cost=15000;call(s);code(s,{{OP_COPY,0,MEM_STATIC,1},{OP_RET}});add(s);

    s=base("sender_bad_signature",v);s.corruption=1;add(s);
    s=base("sender_bad_key",v);s.tx->sender=pubkey_t().get_addr();s.corruption=2;add(s);
    for(unsigned kind:{3,4,5}){s=base("input_crypto_"+std::to_string(kind),v);input(s,10);output(s,10);s.tx->inputs[0].solution=1;s.tx->solutions={nullptr,nullptr};if(kind==4){s.tx->inputs[0].address=pubkey_t().get_addr();s.node.balance_table[{s.tx->inputs[0].address,addr(0)}]=100;}s.corruption=kind;add(s);}
    for(unsigned kind:{3,4}){s=base("user_crypto_"+std::to_string(kind),v);call(s,"run",true);auto op=std::dynamic_pointer_cast<const operation::Execute>(s.tx->execute[0]);auto edited=std::make_shared<operation::Execute>(*op);edited->solution=1;if(kind==4)edited->user=pubkey_t().get_addr();s.tx->execute[0]=edited;s.tx->solutions={nullptr,nullptr};s.corruption=kind;add(s);}
    s=base("input_executable_forbidden",v);txin_t x;x.address=addr(1);x.amount=1;x.solution=0;x.flags=1;s.tx->inputs={x};output(s,1);add(s);
    s=base("input_lookup_budget",v);s.tx->inputs={x};output(s,1);s.tx->max_fee_amount=2300;add(s);
    s=base("user_lookup_budget",v);call(s,"run",true);s.tx->max_fee_amount=13109;add(s);
    s=base("missing_record_index_cost",v);s.node.tx_index[addr(9)].contract_read_cost=5000;code(s,{{OP_CREAD,0,MEM_STATIC,4,7},{OP_RET}});call(s);add(s);
    for(bool existing:{false,true}){s=base(existing?"deposit_zero_balance":"deposit_missing_balance",v);if(existing)s.node.balance_table[{addr(1),addr(0)}]=0;else s.node.balance_table.erase({addr(1),addr(0)});input(s,7);auto op=operation::Deposit::create();op->address=addr(1);op->method="run";op->amount=7;s.tx->execute={op};code(s,{{OP_SEND,0,4,5,6,0},{OP_RET}});add(s);}
    for(unsigned cap:{16000,17000,18000,19000,20000,21000,22000,23000}){s=base("send_budget_"+std::to_string(cap),v);call(s);s.tx->max_fee_amount=cap;code(s,{{OP_COPY,0,MEM_STATIC,1},{OP_SEND,0,4,5,6,7},{OP_RET}});add(s);}
    for(bool nested:{false,true}){s=base(nested?"nested_args":"negative_arg",v);call(s);auto op=std::make_shared<operation::Execute>(*std::dynamic_pointer_cast<const operation::Execute>(s.tx->execute[0]));if(nested){vnx::Object obj;obj["a"]=std::vector<vnx::Variant>{vnx::Variant(uint64_t(1)),vnx::Variant(uint64_t(2))};obj[std::string(1,char(255))]=std::string({char(255),char(0)});op->args={vnx::Variant(obj)};}else op->args={vnx::Variant(int64_t(-1))};s.tx->execute={op};code(s,{{OP_COPY,0,MEM_STATIC,MEM_STACK+1},{OP_RET}});add(s);}
    s=base("raw_memos",v);input(s,10);output(s,10);s.tx->inputs[0].memo=std::string({char(255),char(0)});s.tx->outputs[0].memo=std::string();add(s);
    s=base("implicit_zero_entries",v);s.tx->deploy=d;for(unsigned i=0;i<30;++i){auto cur=addr((i*13)%79+1000);s.node.balance_table[{payer(),cur}]=100;input(s,i%3?i:0,cur);if(i%3==1)output(s,i,cur);}add(s);
    s=base("amount_wrap",v);const uint128 maximum(std::string("340282366920938463463374607431768211455"));s.node.balance_table[{payer(),addr(0)}]=maximum;input(s,maximum-5000);skey_t other;other[31]=2;const auto owner=pubkey_t(other).get_addr();txin_t second;second.address=owner;second.amount=6000;second.solution=1;s.tx->inputs.push_back(second);s.node.balance_table[{owner,addr(0)}]=6000;output(s,999);s.tx->solutions={nullptr,nullptr};s.corruption=6;add(s);
}return out;}
vnx::Object run(Spec s){
    auto backend=std::make_shared<Recorder>();for(const auto&b:s.node.balance_table)backend->set_balance(b.first.first,b.first.second,b.second);
    auto ctx=std::make_shared<ReferenceNode::execution_context_t>();ctx->height=s.height;ctx->storage=std::make_shared<StorageCache>(backend);for(const auto& c:s.node.contracts)ctx->mutate_map[c.first]={s.tx->id};ctx->mutate_map[s.tx->id]={s.tx->id};
    vnx::Object result;result["name"]=s.name;result["height"]=s.height;result["params"]=wire(s.node.params);result["tx"]=wire(s.tx);
    std::vector<vnx::Object> contracts,indexes,balances;for(const auto&c:s.node.contracts){vnx::Object e;e["address"]=hx(c.first);e["wire"]=wire(c.second);e["cost"]=s.node.tx_index.count(c.first)?s.node.tx_index.at(c.first).contract_read_cost:0;contracts.push_back(e);}result["contracts"]=contracts;
    for(const auto&i:s.node.tx_index){vnx::Object e;e["address"]=hx(i.first);e["height"]=i.second.height;e["cost"]=i.second.contract_read_cost;indexes.push_back(e);}result["indexes"]=indexes;
    for(const auto&b:s.node.balance_table){vnx::Object e;e["address"]=hx(b.first.first);e["currency"]=hx(b.first.second);e["amount"]=b.second.str(10);balances.push_back(e);}result["balances"]=balances;
    std::shared_ptr<const exec_result_t> r;
    try{r=s.node.validate(s.tx,ctx);if(!r)r=std::make_shared<exec_result_t>(*s.tx->exec_result);result["accepted"]=true;result["result"]=result_wire(*r);ctx->storage->commit();}
    catch(const static_failure&e){result["accepted"]=false;result["class"]="static";result["message"]=std::string(e.what());}
    catch(const std::exception&e){result["accepted"]=false;result["class"]="dynamic";result["message"]=std::string(e.what());}
    std::vector<vnx::Object>writes;for(const auto&w:backend->writes){vnx::Object e;e["contract"]=hx(w.c);e["address"]=w.a;e["key"]=w.k;auto b=serialize(*w.v,false,true);e["value"]=hex(b.first.get(),b.second);writes.push_back(e);}result["writes"]=writes;
    if(r){auto block=BlockHeader::create();block->height=s.height;result["index_cost"]=s.tx->get_tx_index(s.node.params,block,0).contract_read_cost;auto b=s.node.balance_table;if(!r->did_fail){for(const auto& o:s.tx->outputs)b[{o.address,o.contract}]+=o.amount;for(const auto&o:r->outputs)b[{o.address,o.contract}]+=o.amount;}clamped_sub_assign(b[{*s.tx->sender,addr(0)}],r->total_fee);if(!r->did_fail){for(const auto&i:s.tx->inputs){auto it=b.find({i.address,i.contract});if(it!=b.end())clamped_sub_assign(it->second,i.amount);}for(const auto&i:r->inputs){auto it=b.find({i.address,i.contract});if(it!=b.end())clamped_sub_assign(it->second,i.amount);}}std::vector<vnx::Object>final;for(const auto& e:b){vnx::Object x;x["address"]=hx(e.first.first);x["currency"]=hx(e.first.second);x["amount"]=e.second.str(10);final.push_back(x);}result["settled"]=final;}
    return result;
}
int main(int argc,char**argv){if(argc!=2)return 1;secp256k1_init();std::ofstream output(argv[1]);size_t count=0;for(auto s:cases()){
    const auto original=run(s);output<<vnx::to_string(original)<<'\n';++count;
    if(original["accepted"].to<bool>()){
        const auto raw=vnx::from_hex_string(original["result"].to<std::string>());vnx::VectorInputStream stream(&raw);vnx::TypeInput in(&stream);exec_result_t result;result.read(in,nullptr,nullptr);
        for(unsigned mutation=0;mutation<13;++mutation){auto t=s;t.name+="_result"+std::to_string(mutation);t.tx=std::make_shared<Transaction>(*s.tx);auto r=result;
            if(mutation==1)++r.total_cost;if(mutation==2)++r.total_fee;if(mutation==3)r.did_fail=!r.did_fail;
            if(mutation==4){if(r.error)r.error->code^=1;else{exec_error_t e;e.code=1;r.error=e;}}
            if(mutation==5){if(r.error)r.error->address^=1;else{exec_error_t e;e.address=1;r.error=e;}}
            if(mutation==6){if(r.error)r.error->operation^=1;else{exec_error_t e;e.operation=1;r.error=e;}}
            if(mutation==7){if(r.error){r.error->line=999;r.error->message="different diagnostic";}else{exec_error_t e;e.line=999;e.message="allowed diagnostic";r.error=e;}}
            if(mutation==8)r.error=nullptr;
            if(mutation==9){if(r.outputs.empty()){txout_t o;o.amount=1;r.outputs.push_back(o);}else++r.outputs[0].amount;}
            if(mutation==10){if(r.inputs.empty()){txin_t i;i.amount=1;r.inputs.push_back(i);}else r.inputs[0].flags^=1;}
            if(mutation==11&&!r.inputs.empty())r.inputs[0].solution=7;
            if(mutation==12&&!r.outputs.empty())r.outputs[0].memo=std::string({char(255),char(0)});
            t.tx->update(r,t.node.params);auto trace=run(t);trace["seed"]=s.name;trace["submitted_result"]=result_wire(r);trace["static_cost"]=t.tx->static_cost;trace["content_hash"]=hx(t.tx->content_hash);for(const auto& field:{"contracts","indexes","balances","params","height","tx","result"})trace.field.erase(field);output<<vnx::to_string(trace)<<'\n';++count;
        }
    }
}std::cout<<"Generated "<<count<<" native transaction cases\n";return output?0:1;}
