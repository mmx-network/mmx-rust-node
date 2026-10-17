// Appended to the shared native transaction harness by the generator.
namespace mmx {
struct InlineTasks {
    void add_task(std::function<void()> task) { task(); }
    void sync() {}
};
struct BlockReference : ReferenceNode {
    using ReferenceNode::validate;
    using waitcond_t = int;
    struct Dependencies {
        std::map<addr_t, std::vector<addr_t>> entries;
        bool find(const addr_t& address, std::vector<addr_t>& out) const {
            auto it = entries.find(address);
            if(it == entries.end()) return false;
            out = it->second;
            return true;
        }
    } contract_depends;
    hash_t state_hash;
    std::shared_ptr<BlockHeader> previous;
    vnx::optional<addr_t> expected_payout;
    std::shared_ptr<InlineTasks> threads = std::make_shared<InlineTasks>();
    std::shared_ptr<StorageRAM> storage;
    std::shared_ptr<const BlockHeader> find_prev(std::shared_ptr<const BlockHeader> block) const {
        return block->prev == previous->hash ? previous : nullptr;
    }
    size_t prefetch_balances(const std::set<std::pair<addr_t,addr_t>>&) const { return 0; }
    addr_t get_plot_nft_target(const addr_t&, const vnx::optional<addr_t>&) const {
        throw std::logic_error("NFT reward target resolution is outside this harness");
    }
    vnx::optional<addr_t> get_vdf_reward_winner(std::shared_ptr<const BlockHeader>) const { return expected_payout; }
    std::shared_ptr<execution_context_t> new_exec_context(uint32_t) const;
    void prepare_context(std::shared_ptr<execution_context_t>, std::shared_ptr<const Transaction>) const;
    std::shared_ptr<execution_context_t> validate(std::shared_ptr<const Block>) const;
    std::vector<addr_t> get_all_depends(std::shared_ptr<const contract::Executable>) const;
    std::vector<addr_t> get_all_depends(const addr_t&, uint32_t) const;
    uint64_t calc_block_reward(std::shared_ptr<const BlockHeader>, uint64_t) const;
};
// MMX_BLOCK_METHODS
struct VdfReference : BlockReference {
    std::map<hash_t,std::shared_ptr<BlockHeader>> history;
    std::shared_ptr<const BlockHeader> find_prev(std::shared_ptr<const BlockHeader> block) const {
        auto it=history.find(block->prev);return it==history.end()?nullptr:it->second;
    }
    vnx::optional<addr_t> get_vdf_reward_winner(std::shared_ptr<const BlockHeader>) const;
};
// MMX_VDF_METHOD
}

struct BlockSpec {
    std::string name;
    BlockReference node;
    std::shared_ptr<Block> block;
    std::map<addr_t,uint128> supplies;
    bool isolated = false;
};
BlockSpec block_base(std::string name, unsigned version, unsigned count=2) {
    auto seed = base(name, version);
    BlockSpec s;
    s.name = name + "_v" + std::to_string(version);
    static_cast<ReferenceNode&>(s.node) = seed.node;
    auto p = s.node.params;
    p->transaction_activation = 0;
    p->reward_activation = 0;
    p->vdf_reward_interval = 5;
    p->reward_adjust_interval = 5;
    s.node.previous = BlockHeader::create();
    auto prev = s.node.previous;
    prev->hash = addr(700);
    prev->height = seed.height - 1;
    prev->time_stamp = 100000;
    prev->base_reward = 1000000;
    prev->txfee_buffer = 10000;
    prev->tx_fees = 1001;
    prev->project_addr = addr(7);
    prev->reward_vote_count = 5;
    prev->reward_vote_sum = 1;
    s.node.state_hash = prev->hash;
    auto block = Block::create();
    s.block = block;
    block->prev = prev->hash;
    block->height = seed.height;
    block->time_stamp = prev->time_stamp + p->block_interval_ms;
    block->time_diff = p->time_diff_divider;
    block->space_diff = 1;
    block->vdf_count = 1;
    block->vdf_reward_addr = {addr(8)};
    if(block->height % p->vdf_reward_interval == 0) {
        s.node.expected_payout = addr(8);
        block->vdf_reward_payout = addr(8);
    }
    auto proof = ProofOfSpaceOG::create();
    proof->ksize = 32;
    proof->farmer_key = pubkey_t(secret());
    block->proof = {proof};
    block->reward_addr = addr(6);
    block->project_addr = prev->project_addr;
    block->set_base_reward(p, prev);
    block->txfee_buffer = calc_new_txfee_buffer(p, prev);
    for(unsigned i=0; i<count; ++i) {
        auto tx = std::make_shared<Transaction>(*seed.tx);
        tx->nonce = i+1;
        seal(tx, p);
        block->tx_list.push_back(tx);
    }
    s.supplies[addr(0)] = 10000000;
    return s;
}
std::shared_ptr<Transaction> mutable_tx(BlockSpec& s, unsigned i) {
    auto tx = std::make_shared<Transaction>(*s.block->tx_list.at(i));
    s.block->tx_list[i] = tx;
    return tx;
}
void block_code(BlockSpec& s, std::vector<instr_t> code) { s.node.contracts[addr(101)] = binary(code); }
void block_calls(BlockSpec& s) {
    for(unsigned i=0; i<s.block->tx_list.size(); ++i) {
        auto tx = mutable_tx(s, i);
        auto op = operation::Execute::create();
        op->address = addr(1);
        op->method = "run";
        tx->execute = {op};
    }
}
void refresh_dependencies(BlockSpec& s) {
    for(const auto& pair : s.node.contracts) {
        if(auto exec = std::dynamic_pointer_cast<const contract::Executable>(pair.second)) {
            std::set<addr_t> unique;
            for(const auto& dep : exec->depends) unique.insert(dep.second);
            s.node.contract_depends.entries[pair.first] = {unique.begin(), unique.end()};
        }
    }
}
void sign_block(BlockSpec& s) {
    s.block->hash = s.block->calc_hash(s.node.params);
    s.block->farmer_sig = signature_t::sign(secret(), s.block->hash);
    s.block->content_hash = s.block->calc_content_hash();
}
void materialize(BlockSpec& s) {
    refresh_dependencies(s);
    auto backend = std::make_shared<StorageRAM>();
    for(const auto& b : s.node.balance_table) backend->set_balance(b.first.first,b.first.second,b.second);
    auto ctx = std::make_shared<ReferenceNode::execution_context_t>();
    ctx->height = s.block->height;
    ctx->storage = std::make_shared<StorageCache>(backend);
    for(auto& tx : s.block->tx_list) {
        auto copy = std::make_shared<Transaction>(*tx);
        copy->exec_result = nullptr;
        seal(copy, s.node.params);
        s.node.prepare_context(ctx, copy);
        tx = copy;
    }
    for(unsigned i=0; i<s.block->tx_list.size(); ++i) {
        if(s.isolated) ctx->storage = std::make_shared<StorageCache>(backend);
        auto tx = mutable_tx(s, i);
        auto r = s.node.ReferenceNode::validate(tx, ctx);
        tx->update(*r, s.node.params);
    }
    s.block->finalize(s.node.params);
    s.block->reward_amount = s.block->reward_addr ? s.node.calc_block_reward(s.block,s.block->tx_fees) : 0;
    sign_block(s);
}
vnx::Object native_block(BlockSpec s) {
    auto backend = std::make_shared<Recorder>();
    for(const auto& b : s.node.balance_table) backend->set_balance(b.first.first,b.first.second,b.second);
    s.node.storage = backend;
    vnx::Object out;
    out["name"] = s.name;
    out["params"] = wire(s.node.params);
    out["previous"] = wire(s.node.previous);
    out["block"] = wire(s.block);
    out["expected_payout"] = s.node.expected_payout ? hx(*s.node.expected_payout) : std::string();
    std::vector<vnx::Object> contracts,indexes,balances,supplies;
    for(const auto& c : s.node.contracts) {
        vnx::Object e;e["address"]=hx(c.first);e["wire"]=wire(c.second);e["cost"]=s.node.tx_index.count(c.first)?s.node.tx_index.at(c.first).contract_read_cost:0;contracts.push_back(e);
    }
    for(const auto& i : s.node.tx_index) {
        vnx::Object e;e["address"]=hx(i.first);e["height"]=i.second.height;e["cost"]=i.second.contract_read_cost;indexes.push_back(e);
    }
    for(const auto& b : s.node.balance_table) {
        vnx::Object e;e["address"]=hx(b.first.first);e["currency"]=hx(b.first.second);e["amount"]=b.second.str(10);balances.push_back(e);
    }
    for(const auto& pair : s.supplies) { vnx::Object e;e["currency"]=hx(pair.first);e["amount"]=pair.second.str(10);supplies.push_back(e); }
    out["contracts"]=contracts;out["indexes"]=indexes;out["balances"]=balances;out["supplies"]=supplies;
    try {
        auto ctx=s.node.validate(s.block);
        out["accepted"]=true;
        ctx->storage->commit();
        auto settled=s.node.balance_table;
        std::map<addr_t,uint128_t> deltas;
        std::vector<vnx::Object> inputs,outputs;
        auto io=[](const txio_entry_t& v) {vnx::Object e;e["address"]=hx(v.address);e["currency"]=hx(v.contract);e["amount"]=v.amount.str(10);e["memo_present"]=bool(v.memo);if(v.memo)e["memo"]=hex((const uint8_t*)v.memo->data(),v.memo->size());return e;};
        for(const auto& o:s.block->get_outputs(s.node.params)) {settled[{o.address,o.contract}]+=o.amount;deltas[o.contract]+=o.amount;outputs.push_back(io(o));}
        for(const auto& i:s.block->get_inputs(s.node.params)) {auto it=settled.find({i.address,i.contract});if(it!=settled.end())clamped_sub_assign(it->second,i.amount);clamped_sub_assign(deltas[i.contract],i.amount);inputs.push_back(io(i));}
        out["inputs"]=inputs;out["outputs"]=outputs;
        std::vector<vnx::Object> final,delta,supply,plan;
        for(const auto& b:settled) {vnx::Object e;e["address"]=hx(b.first.first);e["currency"]=hx(b.first.second);e["amount"]=b.second.str(10);final.push_back(e);}
        for(const auto& d:deltas) {vnx::Object e;e["currency"]=hx(d.first);e["amount"]=uint128(d.second).str(10);delta.push_back(e);if(d.second)s.supplies[d.first]+=d.second;}
        for(const auto& d:s.supplies) {vnx::Object e;e["currency"]=hx(d.first);e["amount"]=d.second.str(10);supply.push_back(e);}
        for(const auto& tx:s.block->tx_list) {vnx::Object e;std::vector<std::string> locks,predecessors;for(const auto& pair:ctx->mutate_map)if(std::find(pair.second.begin(),pair.second.end(),tx->id)!=pair.second.end())locks.push_back(hx(pair.first));std::sort(locks.begin(),locks.end());for(const auto& id:ctx->wait_map[tx->id])predecessors.push_back(hx(id));e["locks"]=locks;e["predecessors"]=predecessors;plan.push_back(e);}
        out["settled"]=final;out["supply_delta"]=delta;out["settled_supplies"]=supply;out["plan"]=plan;
    } catch(const std::exception& e) {out["accepted"]=false;out["error"]=std::string(e.what());}
    std::vector<vnx::Object> writes;
    for(const auto& w:backend->writes) {vnx::Object e;e["contract"]=hx(w.c);e["address"]=w.a;e["key"]=w.k;auto b=serialize(*w.v,false,true);e["value"]=hex(b.first.get(),b.second);writes.push_back(e);}
    out["writes"]=writes;
    return out;
}
int main(int argc,char**argv) {
    if(argc!=4)return 1;
    secp256k1_init();
    std::ofstream output_file(argv[1]);
    size_t count=0;
    auto emit=[&](const BlockSpec& s) {output_file<<vnx::to_string(native_block(s))<<'\n';++count;};
    for(unsigned version:{0,1}) {
        for(unsigned n:{0,1,2,3,7}) {auto s=block_base("empty_"+std::to_string(n),version,n);materialize(s);emit(s);}
        auto s=block_base("shared_state",version);
        for(bool isolated:{false,true}) {
            s=block_base(isolated?"forged_isolated_state":"shared_state",version);block_calls(s);
            auto b=binary({{OP_COPY,0,MEM_STATIC,1},{OP_ADD,0,MEM_STATIC,MEM_STATIC,1},{OP_MINT,0,4,MEM_STATIC,0},{OP_RET}});
            auto method=b->methods["run"];method.name="next";method.entry_point=1;b->methods["next"]=method;s.node.contracts[addr(101)]=b;
            auto tx=mutable_tx(s,1);auto op=std::make_shared<operation::Execute>(*std::dynamic_pointer_cast<const operation::Execute>(tx->execute[0]));op->method="next";tx->execute={op};
            s.isolated=isolated;materialize(s);emit(s);
        }
        s=block_base("shared_composite_state",version);block_calls(s);block_code(s,{{OP_COPY,0,MEM_STATIC,MEM_STACK+1},{OP_RET}});
        for(unsigned i=0;i<2;++i) {
            auto tx=mutable_tx(s,i);auto op=std::make_shared<operation::Execute>(*std::dynamic_pointer_cast<const operation::Execute>(tx->execute[0]));
            vnx::Object object;object["items"]=std::vector<vnx::Variant>{vnx::Variant(uint64_t(i+1)),vnx::Variant(uint64_t(i+2))};object[std::string(1,char(255))]=std::string({char(255),char(0)});op->args={vnx::Variant(object)};tx->execute={op};
        }
        materialize(s);emit(s);
        for(unsigned funds:{7,14,100}) {s=block_base("shared_spend_"+std::to_string(funds),version);block_calls(s);block_code(s,{{OP_SEND,0,4,5,6,0},{OP_RET}});s.node.balance_table[{addr(1),addr(0)}]=funds;materialize(s);emit(s);}
        for(bool fail:{false,true}) {
            s=block_base(fail?"child_rollback":"child_shared_spend",version);block_calls(s);
            block_code(s,fail?std::vector<instr_t>{{OP_RCALL,0,2,3,0,0},{OP_FAIL,0,8,19}}:std::vector<instr_t>{{OP_RCALL,0,2,3,0,0},{OP_RET}});
            s.node.contracts[addr(102)]=binary({{OP_COPY,0,MEM_STATIC,1},{OP_SEND,0,4,5,6,0},{OP_RET}});
            auto tx=mutable_tx(s,1);auto op=std::make_shared<operation::Execute>(*std::dynamic_pointer_cast<const operation::Execute>(tx->execute[0]));op->address=addr(2);tx->execute={op};
            s.node.balance_table[{addr(2),addr(0)}]=7;materialize(s);emit(s);
        }
        for(unsigned amount:{7,14}) {
            s=block_base("shared_deposit_"+std::to_string(amount),version);block_calls(s);block_code(s,{{OP_SEND,0,4,5,6,0},{OP_RET}});s.node.balance_table[{addr(1),addr(0)}]=0;
            auto tx=mutable_tx(s,0);txin_t in;in.address=payer();in.amount=amount;in.solution=0;tx->inputs={in};
            auto op=operation::Deposit::create();op->address=addr(1);op->method="run";op->amount=amount;tx->execute={op};materialize(s);emit(s);
        }
        s=block_base("ledger_output_does_not_fund_vm",version);block_calls(s);block_code(s,{{OP_SEND,0,4,5,6,0},{OP_RET}});s.node.balance_table[{addr(1),addr(0)}]=0;
        {auto tx=mutable_tx(s,0);tx->execute.clear();txin_t in;in.address=payer();in.amount=7;in.solution=0;tx->inputs={in};txout_t o;o.address=addr(1);o.amount=7;tx->outputs={o};}materialize(s);emit(s);
        s=block_base("deployment_unavailable_until_apply",version);
        {auto tx=mutable_tx(s,0);auto deploy=contract::Executable::create();deploy->binary=addr(101);tx->deploy=deploy;seal(tx,s.node.params);auto other=mutable_tx(s,1);auto op=operation::Execute::create();op->address=tx->id;op->method="run";other->execute={op};}materialize(s);emit(s);
        s=block_base("forged_isolated_spend",version);block_calls(s);block_code(s,{{OP_SEND,0,4,5,6,0},{OP_RET}});s.node.balance_table[{addr(1),addr(0)}]=7;s.isolated=true;materialize(s);emit(s);
        s=block_base("duplicate",version);materialize(s);s.block->tx_list[1]=s.block->tx_list[0];s.block->finalize(s.node.params);s.block->reward_amount=s.node.calc_block_reward(s.block,s.block->tx_fees);sign_block(s);emit(s);
        for(unsigned ratio:{1024,2048,3072}) {s=block_base("order_"+std::to_string(ratio),version);mutable_tx(s,1)->fee_ratio=ratio;materialize(s);emit(s);}
        for(unsigned balance:{1500,2200,2201,10000}) {s=block_base("fee_reservation_"+std::to_string(balance),version);s.node.balance_table[{payer(),addr(0)}]=balance;materialize(s);emit(s);}
        for(unsigned amount:{400000,1100000}) {s=block_base("input_reservation_"+std::to_string(amount),version);for(unsigned i=0;i<2;++i){auto tx=mutable_tx(s,i);txin_t in;in.address=payer();in.amount=amount;in.solution=0;tx->inputs={in};txout_t o;o.address=payer();o.amount=amount;tx->outputs={o};}materialize(s);emit(s);}
        for(unsigned tier:{1,2,3}) {s=block_base("fee_tier_"+std::to_string(tier),version);s.node.params->min_fee_ratio={1,tier};materialize(s);s.node.params->max_block_size=s.block->static_cost;sign_block(s);emit(s);}
        for(int delta:{-1,0,1}) {s=block_base("tier_rounding_"+std::to_string(delta),version,3);s.node.params->min_fee_ratio={1,1,2};materialize(s);s.node.params->max_block_size=s.block->static_cost+delta;sign_block(s);emit(s);}
        for(unsigned vote:{0,1,2}) {s=block_base("vote_"+std::to_string(vote),version,0);s.block->reward_vote=int(vote)-1;s.block->set_base_reward(s.node.params,s.node.previous);materialize(s);emit(s);}
        for(unsigned mutation=0;mutation<16;++mutation){s=block_base("mutation_"+std::to_string(mutation),version);materialize(s);switch(mutation){case 0:++s.block->tx_count;break;case 1:++s.block->static_cost;break;case 2:++s.block->total_cost;break;case 3:++s.block->tx_fees;break;case 4:s.block->tx_hash=addr(88);break;case 5:++s.block->reward_amount;break;case 6:++s.block->txfee_buffer;break;case 7:++s.block->base_reward;break;case 8:++s.block->reward_vote_count;break;case 9:++s.block->reward_vote_sum;break;case 10:s.block->project_addr=addr(88);break;case 11:s.block->vdf_reward_payout=addr(88);break;case 12:s.node.params->max_block_cost=1;break;case 13:s.node.params->max_block_size=1;break;case 14:s.node.params->transaction_activation=s.block->height+1;break;case 15:s.block->reward_vote=2;break;}sign_block(s);emit(s);}
        s=block_base("mint_supply",version);block_calls(s);block_code(s,{{OP_MINT,0,4,5,0},{OP_RET}});s.supplies[addr(1)]=uint128(std::string("340282366920938463463374607431768211450"));materialize(s);emit(s);
        s=block_base("no_farmer_reward",version);s.block->reward_addr=nullptr;materialize(s);emit(s);
        s=block_base("rewards_not_active",version);s.node.params->reward_activation=s.block->height+1;materialize(s);emit(s);
        s=block_base("duplicate_chain_index",version);materialize(s);s.node.tx_index[s.block->tx_list[0]->id];emit(s);
    }
    std::cout<<"Generated "<<count<<" native block cases\n";
    std::ofstream math(argv[2]);
    size_t math_count=0;
    for(uint64_t fee:{0ull,1ull,2ull,3ull,999ull,1000ull,4294967295ull,9223372036854775807ull,18446744073709551615ull})
    for(unsigned project:{0u,1u,4294967295u})
    for(uint64_t fixed:{0ull,50000ull,18446744073709551615ull})
    for(uint64_t buffer:{0ull,999ull,8640000ull,18446744073709551615ull}) {
        auto s=block_base("math",1,0);auto p=s.node.params;auto prev=s.node.previous;
        p->project_ratio.value=project;p->project_ratio.inverse=3;p->fixed_project_reward=fixed;prev->txfee_buffer=buffer;prev->tx_fees=fee;
        math<<project<<'\t'<<3<<'\t'<<fixed<<'\t'<<p->min_reward<<'\t'<<p->reward_adjust_div<<'\t'<<p->reward_adjust_tick<<'\t'<<p->reward_adjust_interval<<'\t'<<p->reward_activation<<'\t'<<prev->base_reward<<'\t'<<prev->reward_vote_sum<<'\t'<<prev->reward_vote_count<<'\t'<<buffer<<'\t'<<fee<<'\t'<<s.block->height<<'\t'<<1<<'\t'<<fee<<'\t'<<calc_project_reward(p,fee)<<'\t'<<calc_min_reward_deduction(p,buffer)<<'\t'<<calc_new_txfee_buffer(p,prev)<<'\t'<<calc_new_base_reward(p,prev)<<'\t'<<s.node.calc_block_reward(s.block,fee)<<'\n';++math_count;
    }
    for(uint64_t base_reward:{0ull,1ull,9999ull,1000000ull,4200000000000ull,4200000000001ull})
    for(unsigned votes:{0u,1u,2u,5u})
    for(int sum:{-1,0,1})
    for(bool proof:{false,true}) {
        auto s=block_base("math",1,0);auto p=s.node.params;auto prev=s.node.previous;
        prev->base_reward=base_reward;prev->reward_vote_sum=sum;prev->reward_vote_count=votes;
        if(!proof)s.block->proof.clear();
        math<<p->project_ratio.value<<'\t'<<p->project_ratio.inverse<<'\t'<<p->fixed_project_reward<<'\t'<<p->min_reward<<'\t'<<p->reward_adjust_div<<'\t'<<p->reward_adjust_tick<<'\t'<<p->reward_adjust_interval<<'\t'<<p->reward_activation<<'\t'<<base_reward<<'\t'<<sum<<'\t'<<votes<<'\t'<<prev->txfee_buffer<<'\t'<<prev->tx_fees<<'\t'<<s.block->height<<'\t'<<int(proof)<<'\t'<<1001<<'\t'<<calc_project_reward(p,1001)<<'\t'<<calc_min_reward_deduction(p,prev->txfee_buffer)<<'\t'<<calc_new_txfee_buffer(p,prev)<<'\t'<<calc_new_base_reward(p,prev)<<'\t'<<s.node.calc_block_reward(s.block,1001)<<'\n';++math_count;
    }
    std::cout<<"Generated "<<math_count<<" native economic cases\n";
    std::ofstream vdf(argv[3]);
    size_t vdf_count=0;
    for(unsigned interval:{0,1,2,5})for(unsigned length:{0,1,3,5,7})for(unsigned pattern:{0,1,2,3}) {
        VdfReference node;node.params=ChainParams::create();node.params->vdf_reward_interval=interval;
        auto block=BlockHeader::create();block->prev=addr(100);std::vector<std::string> history;
        for(unsigned i=0;i<length;++i) {
            auto h=BlockHeader::create();h->hash=addr(100+i);h->prev=addr(101+i);h->proof_hash=addr(300+i+13*pattern);
            if(pattern==1)h->vdf_reward_addr={addr(1),addr(1),addr(2)};
            if(pattern==2)h->vdf_reward_addr={addr(1),addr(2),addr(3)};
            if(pattern==3)h->vdf_reward_addr={addr((i*3)%5+1),addr(i%5+1)};
            node.history[h->hash]=h;history.push_back(wire(h));
        }
        auto winner=node.get_vdf_reward_winner(block);
        vnx::Object row;row["interval"]=interval;row["history"]=history;row["winner"]=winner?hx(*winner):std::string();vdf<<vnx::to_string(row)<<'\n';++vdf_count;
    }
    std::cout<<"Generated "<<vdf_count<<" native VDF winner cases\n";
    return output_file&&math&&vdf?0:1;
}
