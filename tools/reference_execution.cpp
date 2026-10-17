// Native vm_interface + StorageCache and Node::execute host/transaction rules.
// Fixture generation only; Rust builds never link the reference implementation.
#include <mmx/vm_interface.h>
#include <mmx/vm/Compiler.h>
#include <mmx/vm/StorageRAM.h>
#include <mmx/vm/StorageCache.h>
#include <mmx/contract/Executable.hxx>
#include <mmx/contract/Data.hxx>
#include <mmx/contract/PubKey.hxx>
#include <mmx/contract/MultiSig.hxx>
#include <mmx/contract/WebData.hxx>
#include <mmx/secp256k1.hpp>
#include <vnx/vnx.h>
#include <fstream>
#include <filesystem>
#include <iomanip>
#include <sstream>
using namespace mmx;
using namespace mmx::vm;

std::string hex(const uint8_t* data,size_t size) { std::ostringstream s;s<<std::hex<<std::setfill('0');for(size_t i=0;i<size;++i)s<<std::setw(2)<<unsigned(data[i]);return s.str(); }
template<size_t N> std::string hx(const bytes_t<N>& a) { return hex(a.data(),N); }
std::string raw(const var_t& v) { const auto b=serialize(v,false,true);return hex(b.first.get(),b.second); }
addr_t addr(uint64_t id) { addr_t out;out.from_uint(id);return out; }
std::string wire(std::shared_ptr<const Contract> c) {std::vector<uint8_t>b;vnx::VectorOutputStream s(&b);vnx::TypeOutput out(&s);vnx::write(out,c);out.flush();return hex(b.data(),b.size());}
struct Cell {addr_t c;uint64_t a;vnx::optional<uint64_t> k;std::shared_ptr<var_t> v;};
struct Recorder:StorageRAM {
    std::vector<Cell> writes;
    std::map<std::pair<addr_t,addr_t>,uint128> balances;
    void write(const addr_t& c,uint64_t a,const var_t& v) override {StorageRAM::write(c,a,v);writes.push_back({c,a,nullptr,std::shared_ptr<var_t>(clone(v))});}
    void write(const addr_t& c,uint64_t a,uint64_t k,const var_t& v) override {StorageRAM::write(c,a,k,v);writes.push_back({c,a,k,std::shared_ptr<var_t>(clone(v))});}
    void set_balance(const addr_t& c,const addr_t& k,const uint128& v) override {StorageRAM::set_balance(c,k,v);balances[{c,k}]=v;}
};
struct Call {addr_t address=addr(1);std::string method="run";std::vector<vnx::Variant> args;vnx::optional<addr_t> user;vnx::optional<std::pair<addr_t,uint128>> deposit;};
struct Spec {
    std::string name;uint32_t height=10,hf2=10;uint64_t limit=1000000;uint32_t version=1;
    std::map<addr_t,std::shared_ptr<Contract>> contracts;
    std::map<addr_t,uint64_t> costs;
    std::vector<Call> calls={Call()};std::vector<Cell> initial;
    std::map<std::pair<addr_t,addr_t>,uint128> balances;
    std::set<addr_t> locks={addr(1),addr(2),addr(3)};
    bool deploy=false;
};
auto method(std::string name="run",bool constant=false,uint32_t entry=0) {contract::method_t out;out.name=name;out.is_public=true;out.is_const=constant;out.entry_point=entry;return out;}
auto binary(std::vector<instr_t> code,bool constant=false) {
    auto b=contract::Binary::create();b->name="fixture";
    std::vector<std::unique_ptr<var_t>> values;
    values.push_back(std::make_unique<var_t>());values.push_back(std::make_unique<uint_t>(1));
    values.push_back(to_binary(std::string("dep")));values.push_back(to_binary(std::string("run")));
    values.push_back(to_binary(addr(9)));values.push_back(std::make_unique<uint_t>(7));
    values.push_back(to_binary(addr_t()));values.push_back(to_binary(std::string("memo")));
    values.push_back(to_binary(std::string("name")));values.push_back(to_binary(std::string("failure")));
    values.push_back(to_binary(std::string("a")));values.push_back(to_binary(addr(2)));
    for(const auto& v:values){const auto bytes=serialize(*v,false,false);b->constant.insert(b->constant.end(),bytes.first.get(),bytes.first.get()+bytes.second);}
    b->binary=serialize(code);b->methods["run"]=method("run",constant);b->line_info[0]=10;b->line_info[2]=20;return b;
}
Spec spec(std::string name,std::vector<instr_t> root,std::vector<instr_t> child={{OP_RET}},bool constant=false) {
    Spec s;s.name=name;
    for(uint64_t id:{1,2}) {auto e=contract::Executable::create();e->binary=addr(100+id);e->name="contract"+std::to_string(id);if(id==1)e->depends["dep"]=addr(2);s.contracts[addr(id)]=e;s.costs[addr(id)]=10;s.contracts[e->binary]=binary(id==1?root:child,id==2&&constant);s.costs[e->binary]=20;}
    return s;
}
std::vector<Spec> cases(const std::string& source) {
    const uint32_t T=MEM_STACK,S=MEM_STATIC,E=MEM_EXTERN;
    std::vector<Spec> out;
    auto scalar=spec("scalar",{{OP_COPY,0,S,T+1},{OP_RET}});scalar.calls[0].args={vnx::Variant(41)};out.push_back(scalar);
    vnx::Object obj;obj["a"]=std::vector<vnx::Variant>{vnx::Variant(1),vnx::Variant(2)};
    for(uint32_t h:{9,10}){auto s=scalar;s.name="nested_h"+std::to_string(h);s.height=h;s.calls[0].args={vnx::Variant(obj)};out.push_back(s);}
    for(uint32_t h:{9,10}){auto s=scalar;s.name="negative_h"+std::to_string(h);s.height=h;s.calls[0].args={vnx::Variant(std::vector<vnx::Variant>{vnx::Variant(1),vnx::Variant(-1)})};out.push_back(s);}
    for(uint32_t h:{9,10}){auto s=spec("remote_h"+std::to_string(h),{{OP_COPY,0,T+6,T+1},{OP_RCALL,0,2,3,5,1},{OP_COPY,0,S,T+5},{OP_RET}},{{OP_COPY,0,T,T+1},{OP_RET}},true);s.height=h;s.calls[0].args={vnx::Variant(obj)};out.push_back(s);}
    auto mut=spec("mutator_return_is_cleared",{{OP_COPY,0,T+5,1},{OP_COPY,0,T+6,T+1},{OP_RCALL,0,2,3,5,1},{OP_COPY,0,S,T+5},{OP_RET}},{{OP_COPY,0,S,T+1},{OP_COPY,0,T,5},{OP_RET}});mut.calls[0].args={vnx::Variant(42)};out.push_back(mut);
    auto rollback=mut;rollback.name="child_then_parent_fail";rollback.contracts[addr(101)]=binary({{OP_COPY,0,T+6,T+1},{OP_RCALL,0,2,3,5,1},{OP_FAIL,0,9,17}});out.push_back(rollback);
    auto childfail=mut;childfail.name="child_failure";childfail.contracts[addr(102)]=binary({{OP_COPY,0,S,T+1},{OP_FAIL,0,9,71}});out.push_back(childfail);
    auto twice=mut;twice.name="sibling_reads_committed_child";twice.contracts[addr(101)]=binary({{OP_COPY,0,T+6,T+1},{OP_RCALL,0,2,3,5,1},{OP_RCALL,0,2,8,5,0},{OP_COPY,0,S,T+5},{OP_RET}});
    auto cb=binary({{OP_COPY,0,S,T+1},{OP_RET},{OP_COPY,0,T,S},{OP_RET}});cb->methods["name"]=method("name",true,2);twice.contracts[addr(102)]=cb;out.push_back(twice);
    auto batch=scalar;batch.name="multi_operation_success";batch.calls.push_back(batch.calls[0]);batch.calls[1].args={vnx::Variant(73)};out.push_back(batch);
    auto batchfail=batch;batchfail.name="multi_operation_failure";batchfail.calls[1].method="missing";out.push_back(batchfail);
    auto lock=mut;lock.name="missing_child_lock";lock.locks={addr(1)};out.push_back(lock);
    auto dep=mut;dep.name="missing_dependency_name";auto de=std::make_shared<contract::Executable>(*std::dynamic_pointer_cast<contract::Executable>(dep.contracts[addr(1)]));de->depends.clear();dep.contracts[addr(1)]=de;out.push_back(dep);
    auto priv=scalar;priv.name="private_method";auto pb=binary({{OP_RET}});pb->methods["run"].is_public=false;priv.contracts[addr(101)]=pb;out.push_back(priv);
    auto constcall=scalar;constcall.name="const_discards_writes";constcall.contracts[addr(101)]=binary({{OP_COPY,0,S,1},{OP_RET}},true);out.push_back(constcall);
    auto transfers=spec("child_and_parent_assets",{{OP_RCALL,0,2,3,0,0},{OP_SEND,0,4,5,6,7},{OP_MINT,0,4,5,0},{OP_RET}},{{OP_SEND,0,4,5,6,7},{OP_MINT,0,4,5,0},{OP_RET}});transfers.balances[{addr(1),addr(0)}]=20;transfers.balances[{addr(2),addr(0)}]=20;out.push_back(transfers);
    auto tfail=transfers;tfail.name="assets_rollback";tfail.calls.push_back(Call());tfail.calls[1].method="missing";out.push_back(tfail);
    auto deferred=spec("outputs_are_not_intra_transaction_deposits",{{OP_SEND,0,4,5,6,0},{OP_RET}},{{OP_SEND,0,4,5,6,0},{OP_RET}});deferred.balances[{addr(1),addr(0)}]=20;
    auto eb=std::dynamic_pointer_cast<contract::Binary>(deferred.contracts[addr(101)]);auto ev=read_constants(eb);ev[4]=to_binary(addr(2));eb->constant.clear();for(const auto& v:ev){auto d=serialize(*v,false,false);eb->constant.insert(eb->constant.end(),d.first.get(),d.first.get()+d.second);}deferred.calls.push_back(Call());deferred.calls[1].address=addr(2);out.push_back(deferred);
    auto cc=spec("const_parent_keeps_child_commit",{{OP_RCALL,0,2,3,0,0},{OP_COPY,0,S,1},{OP_RET}},{{OP_COPY,0,S,1},{OP_RET}});std::dynamic_pointer_cast<contract::Binary>(cc.contracts[addr(101)])->methods["run"].is_const=true;out.push_back(cc);
    auto deposit=spec("deposit",{{OP_COPY,0,S,E+EXTERN_DEPOSIT},{OP_BALANCE,0,S+1,6},{OP_SEND,0,4,5,6,0},{OP_RET}});deposit.calls[0].deposit=std::make_pair(addr(0),uint128(10));out.push_back(deposit);
    for(const auto& field:std::vector<std::string>{"name","binary","depends","meta_data","missing","__type"}) {auto s=spec("cread_"+field,{{OP_CREAD,0,S,11,8},{OP_CREAD,0,S+1,11,8},{OP_RET}});auto b=std::dynamic_pointer_cast<contract::Binary>(s.contracts[addr(101)]);auto v=to_binary(field);auto vals=read_constants(b);vals[8]=std::move(v);b->constant.clear();for(const auto& val:vals){auto d=serialize(*val,false,false);b->constant.insert(b->constant.end(),d.first.get(),d.first.get()+d.second);}std::dynamic_pointer_cast<contract::Executable>(s.contracts[addr(2)])->meta_data=std::vector<uint8_t>{1,255,0};out.push_back(s);}
    for(const auto& field:std::vector<std::string>{"methods","build_flags","binary"}) {
        auto s=spec("cread_binary_"+field,{{OP_CREAD,0,S,11,8},{OP_RET}});
        auto b=std::dynamic_pointer_cast<contract::Binary>(s.contracts[addr(101)]);auto vals=read_constants(b);vals[8]=to_binary(field);vals[11]=to_binary(addr(102));b->constant.clear();for(const auto& val:vals){auto d=serialize(*val,false,false);b->constant.insert(b->constant.end(),d.first.get(),d.first.get()+d.second);}out.push_back(s);
    }
    for(const auto& value:std::vector<vnx::Variant>{vnx::Variant(std::vector<vnx::Variant>{vnx::Variant(1),vnx::Variant(-2),vnx::Variant(3)}),vnx::Variant(std::vector<vnx::Variant>{vnx::Variant(1),vnx::Variant(1.5),vnx::Variant(3)})}) {
        auto s=spec("cread_invalid_"+std::to_string(out.size()),{{OP_CREAD,0,S,11,8},{OP_RET}});auto b=std::dynamic_pointer_cast<contract::Binary>(s.contracts[addr(101)]);auto vals=read_constants(b);vals[8]=to_binary(std::string("meta_data"));b->constant.clear();for(const auto& val:vals){auto d=serialize(*val,false,false);b->constant.insert(b->constant.end(),d.first.get(),d.first.get()+d.second);}std::dynamic_pointer_cast<contract::Executable>(s.contracts[addr(2)])->meta_data=value;out.push_back(s);
    }
    auto stored=spec("copy_stored_map_rejected",{{OP_RCALL,0,2,3,0,0},{OP_RET}},{{OP_COPY,0,T,12},{OP_RET}},true);
    auto map=std::make_shared<map_t>();map->address=S;stored.initial.push_back({addr(2),S,nullptr,map});
    auto sb=std::dynamic_pointer_cast<contract::Binary>(stored.contracts[addr(102)]);auto ref=serialize(ref_t(S),false,false);sb->constant.insert(sb->constant.end(),ref.first.get(),ref.first.get()+ref.second);out.push_back(stored);
    std::vector<instr_t> recursive_code={{OP_COPY,0,T+6,1}};for(int i=0;i<101;++i)recursive_code.push_back({OP_CLONE,0,T+6,T+6});recursive_code.push_back({OP_RCALL,0,2,3,5,1});recursive_code.push_back({OP_RET});
    auto recursion=spec("limits_boundary_copy_recursion",recursive_code,{{OP_COPY,0,T,T+1},{OP_RET}},true);out.push_back(recursion);
    for(const auto& field:std::vector<std::string>{"address","owners","payload"}) {
        auto s=spec("cread_override_"+field,{{OP_CREAD,0,S,11,8},{OP_RET}});auto b=std::dynamic_pointer_cast<contract::Binary>(s.contracts[addr(101)]);auto vals=read_constants(b);vals[8]=to_binary(field);b->constant.clear();for(const auto& val:vals){auto d=serialize(*val,false,false);b->constant.insert(b->constant.end(),d.first.get(),d.first.get()+d.second);}
        if(field=="address"){auto c=contract::PubKey::create();c->address=addr(9);s.contracts[addr(2)]=c;}
        if(field=="owners"){auto c=contract::MultiSig::create();c->owners={addr(9),addr(3)};c->num_required=1;s.contracts[addr(2)]=c;}
        if(field=="payload"){auto c=contract::WebData::create();c->mime_type="text/plain";c->payload=std::string({char(0xab),char(255),char(0)});s.contracts[addr(2)]=c;}out.push_back(s);
    }
    auto caller=spec("caller_context",{{OP_RCALL,0,2,3,0,0},{OP_RET}},{{OP_COPY,0,S,E+EXTERN_USER},{OP_COPY,0,S+1,E+EXTERN_ADDRESS},{OP_COPY,0,S+2,E+EXTERN_TXID},{OP_COPY,0,S+3,E+EXTERN_HEIGHT},{OP_RET}});caller.calls[0].user=addr(8);out.push_back(caller);
    auto deploy=scalar;deploy.name="deploy_constructor";deploy.deploy=true;auto e=std::make_shared<contract::Executable>(*std::dynamic_pointer_cast<contract::Executable>(deploy.contracts[addr(1)]));e->init_args={vnx::Variant(99)};e->init_method="run";deploy.contracts[addr(1)]=e;auto db=binary({{OP_COPY,0,S,T+1},{OP_RET}});db->methods["run"].is_public=false;db->methods["run"].is_init=true;deploy.contracts[addr(101)]=db;deploy.calls.clear();out.push_back(deploy);
    auto bytes_args=deploy;bytes_args.name="deploy_binary_args";auto be=std::make_shared<contract::Executable>(*std::dynamic_pointer_cast<contract::Executable>(deploy.contracts[addr(1)]));be->init_args={vnx::Variant(std::vector<uint8_t>{1,255,0})};bytes_args.contracts[addr(1)]=be;out.push_back(bytes_args);
    auto df=deploy;df.name="deploy_rollback";df.calls={Call()};df.calls[0].method="missing";out.push_back(df);
    auto invalid_name=scalar;invalid_name.name="non_utf8_contract_name";auto ib=std::make_shared<contract::Binary>(*std::dynamic_pointer_cast<contract::Binary>(scalar.contracts[addr(101)]));ib->name=std::string(1,char(255));invalid_name.contracts[addr(101)]=ib;out.push_back(invalid_name);
    auto invalid_method=scalar;invalid_method.name="non_utf8_method";auto mb=std::make_shared<contract::Binary>(*std::dynamic_pointer_cast<contract::Binary>(scalar.contracts[addr(101)]));mb->methods[std::string(1,char(255))]=method();invalid_method.contracts[addr(101)]=mb;invalid_method.calls[0].method=std::string(1,char(255));out.push_back(invalid_method);
    auto invalid_dep=mut;invalid_dep.name="non_utf8_dependency";auto ie=std::make_shared<contract::Executable>(*std::dynamic_pointer_cast<contract::Executable>(mut.contracts[addr(1)]));ie->depends.clear();ie->depends[std::string(1,char(254))]=addr(2);invalid_dep.contracts[addr(1)]=ie;auto idb=std::make_shared<contract::Binary>(*std::dynamic_pointer_cast<contract::Binary>(mut.contracts[addr(101)]));auto idv=read_constants(idb);idv[2]=to_binary(std::string(1,char(254)));idb->constant.clear();for(const auto& v:idv){auto d=serialize(*v,false,false);idb->constant.insert(idb->constant.end(),d.first.get(),d.first.get()+d.second);}invalid_dep.contracts[addr(101)]=idb;out.push_back(invalid_dep);
    auto invalid_arg=deploy;invalid_arg.name="non_utf8_constructor_arg";auto ia=std::make_shared<contract::Executable>(*std::dynamic_pointer_cast<contract::Executable>(deploy.contracts[addr(1)]));ia->init_args={vnx::Variant(std::string({char(255),char(0)}))};invalid_arg.contracts[addr(1)]=ia;out.push_back(invalid_arg);
    auto bad=scalar;bad.name="bad_bytecode";auto bb=binary({{OP_RET}});bb->binary={255};bad.contracts[addr(101)]=bb;out.push_back(bad);
    for(const auto& file:std::vector<std::string>{"engine_tests.js","compiler_tests.js"}) {
        auto big=scalar;big.name="limits_original_"+file;big.limit=25000000;
        auto full=std::make_shared<contract::Binary>(*vm::compile_files({source+"/test/vm/"+file}));full->methods["run"]=method();big.contracts[addr(101)]=full;big.calls[0].args.clear();out.push_back(big);
    }
    return out;
}

struct Run {
    const Spec& s;uint64_t cost=0;uint32_t error_code=0,error_pc=uint32_t(-1),operation=uint32_t(-1);vnx::optional<uint32_t> line;
    std::shared_ptr<Recorder> backend=std::make_shared<Recorder>();
    std::shared_ptr<StorageCache> cache;
    std::vector<txout_t> outputs;
    std::map<std::pair<addr_t,addr_t>,uint128> spend;
    Run(const Spec& s):s(s),cache(std::make_shared<StorageCache>(backend)) {for(const auto& c:s.initial)backend->write(c.c,c.a,*c.v);for(const auto& b:s.balances)backend->set_balance(b.first.first,b.first.second,b.second);backend->writes.clear();backend->balances.clear();}
    std::shared_ptr<Contract> fetch(addr_t a,uint64_t& used,uint64_t limit) {
        const uint64_t charge=1000+(s.costs.count(a)?s.costs.at(a):0);
        if(used+charge>limit)throw std::logic_error("not enough gas to read contract");used+=charge;
        auto it=s.contracts.find(a);return it==s.contracts.end()?nullptr:it->second;
    }
    void instance(std::shared_ptr<Engine> vm,std::shared_ptr<contract::Executable> exec,std::string name,bool init,const std::function<void(std::shared_ptr<Engine>)>& setup) {
        if(!s.locks.count(vm->contract))throw std::logic_error("contract not locked");
        auto bin=std::dynamic_pointer_cast<contract::Binary>(fetch(exec->binary,vm->gas_used,vm->gas_limit));if(!bin)throw std::logic_error("no such binary");
        auto m=find_method(bin,name);if(!m)throw std::logic_error("no such method");if(init?!m->is_init:!m->is_public)throw std::logic_error("method visibility");
        load(vm,bin);if(s.height>=s.hf2)setup(vm);
        std::map<addr_t,std::shared_ptr<Contract>> fetched;fetched[s.deploy?addr(1):addr(200)]=s.deploy?s.contracts.at(addr(1)):nullptr;
        vm->remote_call=[&,vm,exec](const std::string& dep,const std::string& method,uint32_t nargs){
            const auto a=exec->get_external(dep);auto& c=fetched[a];if(!c)c=fetch(a,vm->gas_used,vm->gas_limit);auto childexec=std::dynamic_pointer_cast<contract::Executable>(c);if(!childexec)throw std::logic_error("not executable");
            vm->gas_used+=10000;vm->check_gas();
            auto child=std::make_shared<Engine>(a,cache,false,s.version);child->gas_limit=vm->gas_limit-std::min(vm->gas_used,vm->gas_limit);
            const auto stack=vm->get_stack_ptr();
            const auto childsetup=[&](std::shared_ptr<Engine> child){for(uint32_t i=0;i<nargs;++i)copy(child,vm,MEM_STACK+1+i,stack+1+i);child->write(MEM_EXTERN+EXTERN_USER,to_binary(vm->contract));child->write(MEM_EXTERN+EXTERN_ADDRESS,to_binary(a));child->write(MEM_EXTERN+EXTERN_NETWORK,to_binary(std::string("mainnet")));};
            if(s.height<s.hf2)childsetup(child);
            instance(child,childexec,method,false,childsetup);
            copy(vm,child,stack,MEM_STACK);vm->gas_used+=child->gas_used;vm->check_gas();
            child->remote_call={};child->read_contract={};
        };
        vm->read_contract=[&,vm](const addr_t& a,const std::string& field,uint64_t dst){auto& c=fetched[a];if(!c)c=fetch(a,vm->gas_used,vm->gas_limit);if(!c)throw std::logic_error("no such contract");assign(vm,dst,c->read_field(field));vm->check_gas();};
        vm->write(MEM_EXTERN+EXTERN_HEIGHT,uint_t(s.height));vm->write(MEM_EXTERN+EXTERN_TXID,to_binary(s.deploy?addr(1):addr(200)));
        try { execute(vm,*m,true); }
        catch(...) {error_code=vm->error_code;error_pc=vm->error_addr;if(auto n=bin->find_line(error_pc))line=*n;vm->remote_call={};vm->read_contract={};throw;}
        for(const auto& o:vm->outputs){auto amount=cache->get_balance(vm->contract,o.contract);if(!amount||o.amount>*amount)throw std::logic_error("contract over-spend");*amount-=o.amount;cache->set_balance(vm->contract,o.contract,*amount);outputs.push_back(o);spend[{vm->contract,o.contract}]+=o.amount;}
        outputs.insert(outputs.end(),vm->mint_outputs.begin(),vm->mint_outputs.end());vm->remote_call={};vm->read_contract={};
    }
    void call(const Call& c,std::shared_ptr<Contract> contract,bool init=false) {
        auto exec=std::dynamic_pointer_cast<contract::Executable>(contract);if(!exec)throw std::logic_error("not executable");
        auto vm=std::make_shared<Engine>(c.address,cache,false,s.version);vm->gas_limit=s.limit-std::min(cost,s.limit);
        if(c.user){fetch(*c.user,vm->gas_used,vm->gas_limit);vm->write(MEM_EXTERN+EXTERN_USER,to_binary(*c.user));}else vm->write(MEM_EXTERN+EXTERN_USER,var_t());
        vm->write(MEM_EXTERN+EXTERN_ADDRESS,to_binary(c.address));vm->write(MEM_EXTERN+EXTERN_NETWORK,to_binary(std::string("mainnet")));
        if(c.deposit){uint128 amount=c.deposit->second;if(auto v=cache->get_balance(c.address,c.deposit->first))amount+=*v;cache->set_balance(c.address,c.deposit->first,amount);}
        const auto setup=[&](std::shared_ptr<Engine> e){if(c.deposit)set_deposit(e,c.deposit->first,c.deposit->second);set_args(e,c.args);};
        if(s.height<s.hf2)setup(vm);
        std::exception_ptr ex;try{instance(vm,exec,c.method,init,setup);}catch(...){ex=std::current_exception();}
        cost+=std::min(vm->gas_used,vm->gas_limit);if(ex)std::rethrow_exception(ex);
    }
    bool run() {
        try{
            if(s.deploy){auto e=std::dynamic_pointer_cast<contract::Executable>(s.contracts.at(addr(1)));Call c;c.method=e->init_method;c.args=e->init_args;call(c,e,true);}
            for(size_t i=0;i<s.calls.size();++i){operation=i;const auto& c=s.calls[i];auto contract=c.address==addr(1)&&s.deploy?s.contracts.at(addr(1)):fetch(c.address,cost,s.limit);if(!contract)throw std::logic_error("no such contract");call(c,contract);}
            cache->commit();return true;
        }catch(...){return false;}
    }
};
vnx::Object seed(const Spec& s) {
    vnx::Object out;out["name"]=s.name;out["height"]=s.height;out["hf2"]=s.hf2;out["version"]=s.version;out["limit"]=s.limit;out["deploy"]=s.deploy;out["txid"]=hx(s.deploy?addr(1):addr(200));
    std::vector<vnx::Object> contracts;for(const auto& e:s.contracts){vnx::Object c;c["address"]=hx(e.first);c["cost"]=s.costs.count(e.first)?s.costs.at(e.first):0;c["wire"]=wire(e.second);contracts.push_back(c);}out["contracts"]=contracts;
    std::vector<vnx::Object> calls;for(const auto& e:s.calls){vnx::Object c;c["address"]=hx(e.address);c["method_hex"]=hex((const uint8_t*)e.method.data(),e.method.size());c["args"]=e.args;c["user"]=e.user?vnx::Variant(hx(*e.user)):vnx::Variant();if(e.deposit){c["currency"]=hx(e.deposit->first);c["amount"]=e.deposit->second.str(10);}calls.push_back(c);}out["calls"]=calls;
    std::vector<vnx::Object> initial;for(const auto& e:s.initial){vnx::Object w;w["contract"]=hx(e.c);w["address"]=e.a;w["key"]=e.k;w["value"]=raw(*e.v);initial.push_back(w);}out["initial"]=initial;
    std::vector<vnx::Object> balances;for(const auto& e:s.balances){vnx::Object b;b["contract"]=hx(e.first.first);b["currency"]=hx(e.first.second);b["amount"]=e.second.str(10);balances.push_back(b);}out["balances"]=balances;
    std::vector<std::string> locks;for(const auto& a:s.locks)locks.push_back(hx(a));out["locks"]=locks;return out;
}
vnx::Object result(const Spec& s) {
    Run r(s);const bool ok=r.run();vnx::Object out;out["name"]=s.name;out["limit"]=s.limit;out["ok"]=ok;out["cost"]=r.cost;out["code"]=r.error_code;out["pc"]=r.error_pc;out["operation"]=r.operation;out["line"]=r.line;
    std::vector<vnx::Object> writes;for(const auto& c:r.backend->writes){vnx::Object w;w["contract"]=hx(c.c);w["address"]=c.a;w["key"]=c.k;w["value"]=raw(*c.v);writes.push_back(w);}out["writes"]=writes;
    std::vector<vnx::Object> balances;for(const auto& e:r.backend->balances){vnx::Object b;b["contract"]=hx(e.first.first);b["currency"]=hx(e.first.second);b["amount"]=e.second.str(10);balances.push_back(b);}out["balances"]=balances;
    std::vector<vnx::Object> outputs;if(ok)for(const auto& e:r.outputs){vnx::Object o;o["contract"]=hx(e.contract);o["address"]=hx(e.address);o["amount"]=e.amount.str(10);o["memo"]=e.memo?vnx::Variant(hex((const uint8_t*)e.memo->data(),e.memo->size())):vnx::Variant();outputs.push_back(o);}out["outputs"]=outputs;
    std::vector<vnx::Object> spend;if(ok)for(const auto& e:r.spend){vnx::Object b;b["contract"]=hx(e.first.first);b["currency"]=hx(e.first.second);b["amount"]=e.second.str(10);spend.push_back(b);}out["spend"]=spend;return out;
}
int main(int argc,char**argv) {
    if(argc!=3)return 1;secp256k1_init();std::filesystem::create_directories(argv[1]);std::ofstream seeds(std::string(argv[1])+"/seeds.jsonl"),traces(std::string(argv[1])+"/traces.jsonl");size_t count=0;
    for(auto s:cases(argv[2])) {
        seeds<<vnx::to_string(seed(s))<<'\n';const auto baseline=result(s);traces<<vnx::to_string(baseline)<<'\n';++count;
        if(s.name.rfind("limits_",0)==0)continue;
        const auto gas=baseline["cost"].to<uint64_t>();std::set<uint64_t> budgets={0,gas?gas-1:0,gas,gas+1};for(uint64_t i=0;i<gas;i+=257)budgets.insert(i);
        for(const auto budget:budgets){s.limit=budget;traces<<vnx::to_string(result(s))<<'\n';++count;}
    }
    std::cout<<"Generated "<<count<<" contract execution traces\n";return seeds&&traces?0:1;
}
