use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AutoTradeError {
    InvalidAmount = 1,
    Unauthorized = 2,
    SignalNotFound = 3,
    SignalExpired = 4,
    InsufficientBalance = 5,
    InsufficientLiquidity = 6,
    DailyTradeLimitExceeded = 7,
    PositionLimitExceeded = 8,
    StopLossTriggered = 9,
    TradingPaused = 10,
    StrategyNotFound = 11,
    PositionAlreadyExists = 12,
    InsufficientPriceHistory = 13,
    RankingDisabled = 14,
    InvalidBasketSize = 15,
    InvalidPriceData = 16,
    NonCointegratedBasket = 17,
    ActivePortfolioExists = 18,
    NoActivePortfolio = 19,
    NoTradeSignal = 20,
    InvalidStatArbConfig = 21,
    InvalidInsuranceConfig = 22,
    InsuranceNotConfigured = 23,
    SelfReferral = 24,
    ReferralAlreadySet = 25,
    CircularReferral = 26,
    ReferralLimitExceeded = 27,
    SlippageExceeded = 28,
    RoutingPlanNotFound = 29,
    AtomicExecutionFailed = 30,
}
