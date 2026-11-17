SELECT
    p.PlayerID, p.FirstName, p.LastName, p.NameCheck,
    s.Gender
FROM unnest($1::BIGINT[]) WITH ORDINALITY AS u(PlayerID, Idx)
INNER JOIN Players as p ON p.PlayerID = u.PlayerID
INNER JOIN Appearances as s ON p.PlayerID = s.PlayerID
ORDER BY u.Idx;
