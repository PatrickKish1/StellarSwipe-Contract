#![cfg(test)]

use crate::{
    errors::{ContractError, InsufficientBalanceDetail},
    risk_gates::{
        check_user_balance, DEFAULT_ESTIMATED_COPY_TRADE_FEE, MAX_POSITIONS_PER_USER,
    },
    TradeExecutorContract, TradeExecutorContractClient,
};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::Address as _,
    token::StellarAssetClient,
    Address, Env,
};

/// Minimal UserPortfolio: open count + hooks expected by [`TradeExecutorContract::execute_copy_trade`].
#[contract]
pub struct MockUserPortfolio;

#[contracttype]
#[derive(Clone)]
enum MockKey {
    OpenCount(Address),
}

#[contractimpl]
impl MockUserPortfolio {
    pub fn get_open_position_count(env: Env, user: Address) -> u32 {
        env.storage()
            .instance()
            .get(&MockKey::OpenCount(user))
            .unwrap_or(0)
    }

    pub fn record_copy_position(env: Env, user: Address) {
        let key = MockKey::OpenCount(user.clone());
        let c: u32 = env.storage().instance().get(&key).unwrap_or(0);
        env.storage().instance().set(&key, &(c + 1));
    }

    pub fn close_one_copy_position(env: Env, user: Address) {
        let key = MockKey::OpenCount(user);
        let c: u32 = env.storage().instance().get(&key).unwrap_or(0);
        if c > 0 {
            env.storage().instance().set(&key, &(c - 1));
        }
    }
}

const TRADE_AMOUNT: i128 = 1_000_000;

fn sac_token(env: &Env) -> Address {
    let issuer = Address::generate(env);
    let sac = env.register_stellar_asset_contract_v2(issuer);
    sac.address()
}

fn setup_with_balance(user_balance: i128) -> (Env, Address, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let token = sac_token(&env);
    let portfolio_id = env.register(MockUserPortfolio, ());
    let exec_id = env.register(TradeExecutorContract, ());

    StellarAssetClient::new(&env, &token).mint(&user, &user_balance);

    let exec = TradeExecutorContractClient::new(&env, &exec_id);
    exec.initialize(&admin);
    exec.set_user_portfolio(&portfolio_id);

    (env, exec_id, portfolio_id, user, admin, token)
}

#[test]
fn check_user_balance_insufficient() {
    let env = Env::default();
    env.mock_all_auths();
    let user = Address::generate(&env);
    let token = sac_token(&env);
    let amount: i128 = 100;
    let fee: i128 = 10;
    let required = amount + fee;
    StellarAssetClient::new(&env, &token).mint(&user, &(required - 1));

    let err = check_user_balance(&env, &user, &token, amount, fee);
    assert_eq!(
        err,
        Err(InsufficientBalanceDetail {
            required,
            available: required - 1,
        })
    );
}

#[test]
fn check_user_balance_exactly_sufficient() {
    let env = Env::default();
    env.mock_all_auths();
    let user = Address::generate(&env);
    let token = sac_token(&env);
    let amount: i128 = 100;
    let fee: i128 = 10;
    let required = amount + fee;
    StellarAssetClient::new(&env, &token).mint(&user, &required);

    assert!(check_user_balance(&env, &user, &token, amount, fee).is_ok());
}

#[test]
fn check_user_balance_more_than_sufficient() {
    let env = Env::default();
    env.mock_all_auths();
    let user = Address::generate(&env);
    let token = sac_token(&env);
    let amount: i128 = 100;
    let fee: i128 = 10;
    let required = amount + fee;
    StellarAssetClient::new(&env, &token).mint(&user, &(required + 1_000_000));

    assert!(check_user_balance(&env, &user, &token, amount, fee).is_ok());
}

#[test]
fn execute_copy_trade_insufficient_balance_sets_detail() {
    let required = TRADE_AMOUNT + DEFAULT_ESTIMATED_COPY_TRADE_FEE;
    let (env, exec_id, _pf, user, _admin, token) = setup_with_balance(required - 1);
    let exec = TradeExecutorContractClient::new(&env, &exec_id);

    let err = env.as_contract(&exec_id, || {
        crate::TradeExecutorContract::execute_copy_trade(
            env.clone(),
            user.clone(),
            token.clone(),
            TRADE_AMOUNT,
        )
    });
    assert_eq!(err, Err(ContractError::InsufficientBalance));

    let detail = exec.get_insufficient_balance_detail(&user).unwrap();
    assert_eq!(
        detail,
        InsufficientBalanceDetail {
            required,
            available: required - 1,
        }
    );
}

#[test]
fn execute_copy_trade_sufficient_balance_invokes_portfolio() {
    let per = TRADE_AMOUNT + DEFAULT_ESTIMATED_COPY_TRADE_FEE;
    let (env, exec_id, portfolio_id, user, _admin, token) = setup_with_balance(per + 1_000_000);
    let exec = TradeExecutorContractClient::new(&env, &exec_id);
    exec.execute_copy_trade(&user, &token, &TRADE_AMOUNT);
    assert!(exec.get_insufficient_balance_detail(&user).is_none());
    assert_eq!(
        MockUserPortfolioClient::new(&env, &portfolio_id).get_open_position_count(&user),
        1
    );
}

#[test]
fn execute_copy_trade_zero_amount_invalid() {
    let (env, exec_id, _pf, user, _admin, token) = setup_with_balance(1_000_000_000);
    let err = env.as_contract(&exec_id, || {
        crate::TradeExecutorContract::execute_copy_trade(
            env.clone(),
            user.clone(),
            token.clone(),
            0,
        )
    });
    assert_eq!(err, Err(ContractError::InvalidAmount));
}

#[test]
fn twenty_first_copy_trade_fails_until_one_closed() {
    let per = TRADE_AMOUNT + DEFAULT_ESTIMATED_COPY_TRADE_FEE;
    let (env, exec_id, portfolio_id, user, _admin, token) =
        setup_with_balance(per * 30 + 1_000_000);
    let exec = TradeExecutorContractClient::new(&env, &exec_id);

    for _ in 0..MAX_POSITIONS_PER_USER {
        exec.execute_copy_trade(&user, &token, &TRADE_AMOUNT);
    }

    let err = env.as_contract(&exec_id, || {
        crate::TradeExecutorContract::execute_copy_trade(
            env.clone(),
            user.clone(),
            token.clone(),
            TRADE_AMOUNT,
        )
    });
    assert_eq!(err, Err(ContractError::PositionLimitReached));

    MockUserPortfolioClient::new(&env, &portfolio_id).close_one_copy_position(&user);

    exec.execute_copy_trade(&user, &token, &TRADE_AMOUNT);

    let mock = MockUserPortfolioClient::new(&env, &portfolio_id);
    assert_eq!(mock.get_open_position_count(&user), MAX_POSITIONS_PER_USER);
}

#[test]
fn whitelisted_user_bypasses_position_limit() {
    let per = TRADE_AMOUNT + DEFAULT_ESTIMATED_COPY_TRADE_FEE;
    let (env, exec_id, portfolio_id, user, _admin, token) =
        setup_with_balance(per * 35 + 1_000_000);
    let exec = TradeExecutorContractClient::new(&env, &exec_id);

    for _ in 0..MAX_POSITIONS_PER_USER {
        exec.execute_copy_trade(&user, &token, &TRADE_AMOUNT);
    }

    let err = env.as_contract(&exec_id, || {
        crate::TradeExecutorContract::execute_copy_trade(
            env.clone(),
            user.clone(),
            token.clone(),
            TRADE_AMOUNT,
        )
    });
    assert_eq!(err, Err(ContractError::PositionLimitReached));

    exec.set_position_limit_exempt(&user, &true);
    assert!(exec.is_position_limit_exempt(&user));

    exec.execute_copy_trade(&user, &token, &TRADE_AMOUNT);

    let mock = MockUserPortfolioClient::new(&env, &portfolio_id);
    assert_eq!(mock.get_open_position_count(&user), MAX_POSITIONS_PER_USER + 1);

    exec.set_position_limit_exempt(&user, &false);
    assert!(!exec.is_position_limit_exempt(&user));

    let err2 = env.as_contract(&exec_id, || {
        crate::TradeExecutorContract::execute_copy_trade(
            env.clone(),
            user.clone(),
            token.clone(),
            TRADE_AMOUNT,
        )
    });
    assert_eq!(err2, Err(ContractError::PositionLimitReached));
}
