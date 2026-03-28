#![no_std]

mod analytics;

pub use analytics::{AnalyticsPeriod, FeeAnalytics};

use soroban_sdk::token::Client as TokenClient;
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, panic_with_error, Address,
    Env, MuxedAddress, Vec,
};

/// Protocol fee rate: 0.1% = 10 basis points.
const FEE_RATE_BPS: i128 = 10;
const BPS_DENOM: i128 = 10_000;
/// When `trade_amount * FEE_RATE_BPS < BPS_DENOM`, charge at least one stroop.
const MIN_FEE_STROOPS: i128 = 1;

/// Default share of each fee credited to the signal provider (basis points). 5000 = 50%.
const DEFAULT_PROVIDER_FEE_SHARE_BPS: u32 = 5_000;

/// Maximum fee-exempt addresses (instance storage cap).
const MAX_FEE_EXEMPT_ADDRESSES: u32 = 100;

#[contract]
pub struct FeeCollector;

/// Composite storage key for per-provider, per-token pending balances.
#[contracttype]
#[derive(Clone)]
pub struct ProviderPendingKey {
    pub provider: Address,
    pub token: Address,
}

#[contracttype]
#[derive(Clone)]
pub enum StorageKey {
    AccumulatedFees(Address),
    Admin,
    Treasury,
    ProviderFeeShareBps,
    /// Admin-managed fee-exempt trading addresses (max [`MAX_FEE_EXEMPT_ADDRESSES`] entries).
    FeeExemptList,
    /// Running total of `provider_share` not yet claimed (per provider + token).
    ProviderPendingFees(ProviderPendingKey),
    /// Temporary analytics bucket: fees for UTC day `timestamp / 86400` (~30-day TTL).
    DailyFees(u64),
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    NonPositiveTrade = 3,
    Overflow = 4,
    /// `provider_fee_share_bps` must be in `0..=10_000`.
    InvalidProviderFeeShareBps = 5,
    /// Fee-exempt list already holds [`MAX_FEE_EXEMPT_ADDRESSES`] entries.
    FeeExemptListFull = 6,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeCollected {
    #[topic]
    pub payer: Address,
    #[topic]
    pub token: Address,
    pub amount: i128,
    pub trade_amount: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeDistributed {
    #[topic]
    pub provider: Address,
    #[topic]
    pub token: Address,
    pub provider_share: i128,
    pub treasury_share: i128,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeExemptAdded {
    #[topic]
    pub address: Address,
}

#[contractevent]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeExemptRemoved {
    #[topic]
    pub address: Address,
}

fn load_fee_exempt_list(env: &Env) -> Vec<Address> {
    env.storage()
        .instance()
        .get(&StorageKey::FeeExemptList)
        .unwrap_or_else(|| Vec::<Address>::new(env))
}

fn fee_exempt_list_contains(list: &Vec<Address>, addr: &Address) -> bool {
    let len = list.len();
    let mut i: u32 = 0;
    while i < len {
        if list.get(i).unwrap() == *addr {
            return true;
        }
        i += 1;
    }
    false
}

#[contractimpl]
impl FeeCollector {
    pub fn initialize(env: Env, admin: Address, treasury: Address) {
        if env.storage().instance().has(&StorageKey::Admin) {
            panic_with_error!(&env, Error::AlreadyInitialized);
        }
        env.storage().instance().set(&StorageKey::Admin, &admin);
        env.storage().instance().set(&StorageKey::Treasury, &treasury);
        env.storage()
            .instance()
            .set(&StorageKey::ProviderFeeShareBps, &DEFAULT_PROVIDER_FEE_SHARE_BPS);
        let empty_exempt: Vec<Address> = Vec::new(&env);
        env.storage()
            .instance()
            .set(&StorageKey::FeeExemptList, &empty_exempt);
    }

    pub fn add_fee_exempt(env: Env, address: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        admin.require_auth();
        let mut list = load_fee_exempt_list(&env);
        if fee_exempt_list_contains(&list, &address) {
            return;
        }
        if list.len() >= MAX_FEE_EXEMPT_ADDRESSES {
            panic_with_error!(&env, Error::FeeExemptListFull);
        }
        list.push_back(address.clone());
        env.storage().instance().set(&StorageKey::FeeExemptList, &list);
        FeeExemptAdded { address }.publish(&env);
    }

    pub fn remove_fee_exempt(env: Env, address: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        admin.require_auth();
        let list = load_fee_exempt_list(&env);
        if !fee_exempt_list_contains(&list, &address) {
            return;
        }
        let mut new_list: Vec<Address> = Vec::new(&env);
        let len = list.len();
        let mut i: u32 = 0;
        while i < len {
            let a = list.get(i).unwrap();
            if a != address {
                new_list.push_back(a);
            }
            i += 1;
        }
        env.storage()
            .instance()
            .set(&StorageKey::FeeExemptList, &new_list);
        FeeExemptRemoved { address }.publish(&env);
    }

    pub fn get_fee_exempt_list(env: Env) -> Vec<Address> {
        load_fee_exempt_list(&env)
    }

    /// Sets the provider’s share of each fee in basis points (0–10_000 = 0–100%).
    pub fn set_provider_fee_share_bps(env: Env, provider_fee_share_bps: u32) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&StorageKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));
        admin.require_auth();
        if provider_fee_share_bps > 10_000 {
            panic_with_error!(&env, Error::InvalidProviderFeeShareBps);
        }
        env.storage()
            .instance()
            .set(&StorageKey::ProviderFeeShareBps, &provider_fee_share_bps);
    }

    pub fn get_provider_fee_share_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&StorageKey::ProviderFeeShareBps)
            .unwrap_or(DEFAULT_PROVIDER_FEE_SHARE_BPS)
    }

    /// Pulls fee from `payer`, sends treasury portion to treasury, accrues provider portion for `claim_provider_fees`.
    /// Returns total fee charged (0 if `payer` is fee-exempt).
    pub fn collect_fee(
        env: Env,
        payer: Address,
        trade_amount: i128,
        token: Address,
        provider: Address,
    ) -> i128 {
        payer.require_auth();

        if trade_amount <= 0 {
            panic_with_error!(&env, Error::NonPositiveTrade);
        }

        let exempt_list = load_fee_exempt_list(&env);
        let fee_exempt = fee_exempt_list_contains(&exempt_list, &payer);

        let fee = if fee_exempt {
            0
        } else {
            let product = trade_amount
                .checked_mul(FEE_RATE_BPS)
                .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow));
            if product < BPS_DENOM {
                MIN_FEE_STROOPS
            } else {
                product / BPS_DENOM
            }
        };

        if fee > 0 {
            let share_bps = Self::get_provider_fee_share_bps(env.clone()) as i128;
            let provider_share = fee
                .checked_mul(share_bps)
                .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow))
                / BPS_DENOM;
            let treasury_share = fee
                .checked_sub(provider_share)
                .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow));

            let token_client = TokenClient::new(&env, &token);
            let this = env.current_contract_address();
            token_client.transfer(&payer, &MuxedAddress::from(&this), &fee);

            let treasury: Address = env
                .storage()
                .instance()
                .get(&StorageKey::Treasury)
                .unwrap_or_else(|| panic_with_error!(&env, Error::NotInitialized));

            if treasury_share > 0 {
                token_client.transfer(&this, &MuxedAddress::from(&treasury), &treasury_share);
            }

            if provider_share > 0 {
                let pending_key = ProviderPendingKey {
                    provider: provider.clone(),
                    token: token.clone(),
                };
                let prev: i128 = env
                    .storage()
                    .persistent()
                    .get(&StorageKey::ProviderPendingFees(pending_key.clone()))
                    .unwrap_or(0);
                let next = prev
                    .checked_add(provider_share)
                    .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow));
                env.storage()
                    .persistent()
                    .set(&StorageKey::ProviderPendingFees(pending_key), &next);
            }

            let prev_total: i128 = env
                .storage()
                .persistent()
                .get(&StorageKey::AccumulatedFees(token.clone()))
                .unwrap_or(0);
            let next_total = prev_total
                .checked_add(fee)
                .unwrap_or_else(|| panic_with_error!(&env, Error::Overflow));
            env.storage()
                .persistent()
                .set(&StorageKey::AccumulatedFees(token.clone()), &next_total);

            FeeDistributed {
                provider: provider.clone(),
                token: token.clone(),
                provider_share,
                treasury_share,
            }
            .publish(&env);

            analytics::record_daily_fee_collection(&env, fee, token.clone());
        }

        FeeCollected {
            payer: payer.clone(),
            token: token.clone(),
            amount: fee,
            trade_amount,
        }
        .publish(&env);

        fee
    }

    /// Transfers this contract’s pending provider share for `token` to `provider`.
    pub fn claim_provider_fees(env: Env, provider: Address, token: Address) -> i128 {
        provider.require_auth();
        let pending_key = ProviderPendingKey {
            provider: provider.clone(),
            token: token.clone(),
        };
        let pending: i128 = env
            .storage()
            .persistent()
            .get(&StorageKey::ProviderPendingFees(pending_key.clone()))
            .unwrap_or(0);
        if pending <= 0 {
            return 0;
        }

        let token_client = TokenClient::new(&env, &token);
        let this = env.current_contract_address();
        token_client.transfer(&this, &MuxedAddress::from(&provider), &pending);
        env.storage()
            .persistent()
            .remove(&StorageKey::ProviderPendingFees(pending_key));

        pending
    }

    pub fn get_provider_pending_fees(env: Env, provider: Address, token: Address) -> i128 {
        let pending_key = ProviderPendingKey { provider, token };
        env.storage()
            .persistent()
            .get(&StorageKey::ProviderPendingFees(pending_key))
            .unwrap_or(0)
    }

    pub fn get_accumulated_fees(env: Env, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&StorageKey::AccumulatedFees(token))
            .unwrap_or(0)
    }

    pub fn get_fee_analytics(env: Env, period: AnalyticsPeriod) -> FeeAnalytics {
        analytics::get_fee_analytics(&env, period)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::analytics;
    use soroban_sdk::testutils::storage::Temporary as _;
    use soroban_sdk::testutils::{Address as _, Ledger as _};
    use soroban_sdk::token::{Client as TokenClient, StellarAssetClient};

    /// 7 decimals (e.g. XLM-style) for “100 XLM” style tests.
    const STROOPS_PER_UNIT: i128 = 10_000_000;

    fn setup_fee_collector(
        env: &Env,
    ) -> (
        Address,
        Address,
        Address,
        Address,
        FeeCollectorClient<'_>,
    ) {
        env.mock_all_auths();
        let admin = Address::generate(env);
        let treasury = Address::generate(env);
        let contract_id = env.register(FeeCollector, ());
        let client = FeeCollectorClient::new(env, &contract_id);
        client.initialize(&admin, &treasury);
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let token = sac.address();
        let payer = Address::generate(env);
        StellarAssetClient::new(env, &token).mint(&payer, &100_000_000_000i128);
        (admin, treasury, token, payer, client)
    }

    #[test]
    fn normal_trade_charges_exactly_10_bps() {
        let env = Env::default();
        let (_admin, treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);
        let trade_amount = 1_000_000i128;
        let fee = client.collect_fee(&payer, &trade_amount, &token, &provider);
        assert_eq!(fee, 1_000);
        let contract_id = client.address.clone();
        assert_eq!(TokenClient::new(&env, &token).balance(&contract_id), 500);
        assert_eq!(TokenClient::new(&env, &token).balance(&treasury), 500);
        assert_eq!(client.get_provider_pending_fees(&provider, &token), 500);
        assert_eq!(client.get_accumulated_fees(&token), 1_000);
    }

    #[test]
    fn dust_trade_charges_minimum_one_stroop() {
        let env = Env::default();
        let (_admin, treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);
        let trade_amount = 999i128;
        assert!(trade_amount * FEE_RATE_BPS < BPS_DENOM);
        let fee = client.collect_fee(&payer, &trade_amount, &token, &provider);
        assert_eq!(fee, 1);
        let contract_id = client.address.clone();
        assert_eq!(TokenClient::new(&env, &token).balance(&contract_id), 0);
        assert_eq!(TokenClient::new(&env, &token).balance(&treasury), 1);
        assert_eq!(client.get_provider_pending_fees(&provider, &token), 0);
        assert_eq!(client.get_accumulated_fees(&token), 1);
    }

    #[test]
    fn fee_exempt_address_pays_no_fee() {
        let env = Env::default();
        let (_admin, treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);
        client.add_fee_exempt(&payer);
        let trade_amount = 1_000_000i128;
        let fee = client.collect_fee(&payer, &trade_amount, &token, &provider);
        assert_eq!(fee, 0);
        let contract_id = client.address.clone();
        assert_eq!(TokenClient::new(&env, &token).balance(&contract_id), 0);
        assert_eq!(TokenClient::new(&env, &token).balance(&treasury), 0);
        assert_eq!(client.get_provider_pending_fees(&provider, &token), 0);
        assert_eq!(client.get_accumulated_fees(&token), 0);
    }

    #[test]
    fn remove_fee_exempt_restores_normal_fees() {
        let env = Env::default();
        let (_admin, treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);
        client.add_fee_exempt(&payer);
        assert_eq!(client.get_fee_exempt_list().len(), 1);
        client.remove_fee_exempt(&payer);
        assert_eq!(client.get_fee_exempt_list().len(), 0);

        let trade_amount = 1_000_000i128;
        let fee = client.collect_fee(&payer, &trade_amount, &token, &provider);
        assert_eq!(fee, 1_000);
        assert_eq!(TokenClient::new(&env, &token).balance(&treasury), 500);
    }

    #[test]
    fn duplicate_add_fee_exempt_is_idempotent() {
        let env = Env::default();
        let (_admin, _treasury, _token, payer, client) = setup_fee_collector(&env);
        client.add_fee_exempt(&payer);
        client.add_fee_exempt(&payer);
        assert_eq!(client.get_fee_exempt_list().len(), 1);
    }

    #[test]
    fn remove_fee_exempt_unknown_is_noop() {
        let env = Env::default();
        let (_admin, _treasury, _token, payer, client) = setup_fee_collector(&env);
        let stranger = Address::generate(&env);
        client.remove_fee_exempt(&stranger);
        assert_eq!(client.get_fee_exempt_list().len(), 0);
        client.add_fee_exempt(&payer);
        client.remove_fee_exempt(&stranger);
        assert_eq!(client.get_fee_exempt_list().len(), 1);
    }

    #[test]
    fn fee_exempt_list_cap_enforced_at_100() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let contract_id = env.register(FeeCollector, ());
        let client = FeeCollectorClient::new(&env, &contract_id);
        client.initialize(&admin, &treasury);

        for _ in 0..100 {
            client.add_fee_exempt(&Address::generate(&env));
        }
        assert_eq!(client.get_fee_exempt_list().len(), 100);
        let res = client.try_add_fee_exempt(&Address::generate(&env));
        assert!(res.is_err());
    }

    #[test]
    fn hundred_xlm_trade_50_50_split() {
        let env = Env::default();
        let (_admin, treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);
        assert_eq!(client.get_provider_fee_share_bps(), 5_000);

        let trade_amount = 100 * STROOPS_PER_UNIT;
        let fee = client.collect_fee(&payer, &trade_amount, &token, &provider);
        // 0.1% of 100 XLM = 0.1 XLM = 1_000_000 stroops; 50/50 → 0.05 XLM = 500_000 stroops each.
        let expected_fee = 1_000_000i128;
        let expected_half = 500_000i128;
        assert_eq!(fee, expected_fee);
        assert_eq!(
            TokenClient::new(&env, &token).balance(&treasury),
            expected_half,
            "treasury should receive 0.05 XLM"
        );
        assert_eq!(
            client.get_provider_pending_fees(&provider, &token),
            expected_half,
            "provider pending should be 0.05 XLM before claim"
        );

        let claimed = client.claim_provider_fees(&provider, &token);
        assert_eq!(claimed, expected_half);
        assert_eq!(
            TokenClient::new(&env, &token).balance(&provider),
            expected_half,
            "provider should receive 0.05 XLM on claim"
        );
        assert_eq!(client.get_provider_pending_fees(&provider, &token), 0);
        let contract_id = client.address.clone();
        assert_eq!(TokenClient::new(&env, &token).balance(&contract_id), 0);
    }

    #[test]
    fn set_provider_fee_share_bps_rejects_over_100_percent() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let contract_id = env.register(FeeCollector, ());
        let client = FeeCollectorClient::new(&env, &contract_id);
        client.initialize(&admin, &treasury);
        let res = client.try_set_provider_fee_share_bps(&10_001u32);
        assert!(res.is_err());
    }

    #[test]
    fn fee_analytics_daily_accumulates_same_day() {
        let env = Env::default();
        let (_admin, _treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);
        let base_day: u64 = 12_345;
        env.ledger()
            .set_timestamp(base_day * analytics::SECONDS_PER_DAY + 100);

        let trade_amount = 1_000_000i128;
        client.collect_fee(&payer, &trade_amount, &token, &provider);
        client.collect_fee(&payer, &trade_amount, &token, &provider);

        let a = client.get_fee_analytics(&AnalyticsPeriod::Daily);
        assert_eq!(a.total_fees, 2_000);
        assert_eq!(a.trade_count, 2);
        assert_eq!(a.avg_fee_per_trade, 1_000);
        assert_eq!(a.top_token, token);
    }

    #[test]
    fn fee_analytics_ten_days_weekly_sums_last_seven() {
        let env = Env::default();
        let (_admin, _treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);
        const BASE_DAY: u64 = 50_000;
        const FEE: i128 = 1_000;
        let trade_amount = 1_000_000i128;

        for d in 0..10_u64 {
            env.ledger().set_timestamp(BASE_DAY * analytics::SECONDS_PER_DAY + d * analytics::SECONDS_PER_DAY + 200);
            assert_eq!(
                client.collect_fee(&payer, &trade_amount, &token, &provider),
                FEE
            );
        }

        env.ledger().set_timestamp(BASE_DAY * analytics::SECONDS_PER_DAY + 9 * analytics::SECONDS_PER_DAY + 200);

        let w = client.get_fee_analytics(&AnalyticsPeriod::Weekly);
        assert_eq!(w.total_fees, 7 * FEE);
        assert_eq!(w.trade_count, 7);
        assert_eq!(w.avg_fee_per_trade, FEE);
        assert_eq!(w.top_token, token);

        let d = client.get_fee_analytics(&AnalyticsPeriod::Daily);
        assert_eq!(d.total_fees, FEE);
        assert_eq!(d.trade_count, 1);

        let m = client.get_fee_analytics(&AnalyticsPeriod::Monthly);
        assert_eq!(m.total_fees, 10 * FEE);
        assert_eq!(m.trade_count, 10);
    }

    #[test]
    fn fee_analytics_skips_exempt_trades() {
        let env = Env::default();
        let (_admin, _treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);
        env.ledger().set_timestamp(60_000 * analytics::SECONDS_PER_DAY + 50);
        client.collect_fee(&payer, &1_000_000i128, &token, &provider);
        client.add_fee_exempt(&payer);
        client.collect_fee(&payer, &1_000_000i128, &token, &provider);

        let a = client.get_fee_analytics(&AnalyticsPeriod::Daily);
        assert_eq!(a.total_fees, 1_000);
        assert_eq!(a.trade_count, 1);
    }

    #[test]
    fn daily_fee_temporary_bucket_ttl_set() {
        let env = Env::default();
        let (_admin, _treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);
        env.ledger().set_timestamp(70_000 * analytics::SECONDS_PER_DAY + 99);
        client.collect_fee(&payer, &1_000_000i128, &token, &provider);

        let day = env.ledger().timestamp() / analytics::SECONDS_PER_DAY;
        let cid = client.address.clone();
        let ttl = env.as_contract(&cid, || env.storage().temporary().get_ttl(&StorageKey::DailyFees(day)));
        assert!(ttl > 0);
        assert!(ttl <= analytics::TEMP_FEE_BUCKET_TTL_LEDGERS);
    }

    #[test]
    fn daily_fee_temporary_bucket_expires_past_ttl_ledgers() {
        let env = Env::default();
        let (_admin, _treasury, token, payer, client) = setup_fee_collector(&env);
        let provider = Address::generate(&env);

        let mut li = env.ledger().get();
        li.max_entry_ttl = 2_000_000;
        env.ledger().set(li.clone());

        env.ledger().set_timestamp(80_000 * analytics::SECONDS_PER_DAY + 1);
        client.collect_fee(&payer, &1_000_000i128, &token, &provider);
        let day = env.ledger().timestamp() / analytics::SECONDS_PER_DAY;
        let cid = client.address.clone();

        env.as_contract(&cid, || {
            assert!(env.storage().temporary().has(&StorageKey::DailyFees(day)));
        });

        li.sequence_number = li
            .sequence_number
            .saturating_add(analytics::TEMP_FEE_BUCKET_TTL_LEDGERS)
            .saturating_add(50_000);
        env.ledger().set(li);

        env.as_contract(&cid, || {
            assert!(!env.storage().temporary().has(&StorageKey::DailyFees(day)));
        });
    }
}
