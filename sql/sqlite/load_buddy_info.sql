SELECT
    p.PlayerID, p.FirstName, p.LastName, p.NameCheck,
    s.Gender
FROM json_each(?1) AS u
INNER JOIN Players as p ON p.PlayerID = u.value
INNER JOIN Appearances as s ON p.PlayerID = s.PlayerID
ORDER BY u.key;
